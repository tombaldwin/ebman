//! S3: application-bundle upload, including the multipart path for
//! bundles past the threshold.

use super::*;

/// Switch to multipart upload when the bundle is at least this large.
/// 64 MiB is well above the SDK's default chunked-read window so the
/// streaming PutObject path handles "normal" bundles comfortably; above
/// this threshold the multipart path's recoverability (partial parts can
/// be retried) starts to matter, and the 5 GiB single-PutObject ceiling
/// looms.
pub const MULTIPART_THRESHOLD: u64 = 64 * 1024 * 1024;

/// Per-part chunk size for multipart uploads. S3's minimum part size is
/// 5 MiB (except the last part); 16 MiB gives us 320 GiB headroom under
/// the 10,000-part ceiling, well above S3's 5 TiB object cap.
pub const MULTIPART_PART_SIZE: u64 = 16 * 1024 * 1024;

/// Decide whether a bundle of `size` bytes should go through multipart.
/// Pure for tests; production code calls this with [`MULTIPART_THRESHOLD`].
pub fn should_multipart(size: u64, threshold: u64) -> bool {
    size >= threshold
}

/// Plan the per-part lengths for a multipart upload of a file of
/// `total_size` bytes using `part_size` bytes per part. The last part is
/// whatever's left (>= 1 byte, < part_size) unless `total_size` is an
/// exact multiple. Empty input (`total_size == 0`) yields an empty plan.
/// Pure — for tests and for the upload loop itself.
pub fn plan_part_lengths(total_size: u64, part_size: u64) -> Vec<u64> {
    if total_size == 0 || part_size == 0 {
        return Vec::new();
    }
    let full = total_size / part_size;
    let remainder = total_size % part_size;
    let mut out = Vec::with_capacity(full as usize + if remainder > 0 { 1 } else { 0 });
    for _ in 0..full {
        out.push(part_size);
    }
    if remainder > 0 {
        out.push(remainder);
    }
    out
}

impl AwsClient {
    /// Best-effort `AbortMultipartUpload`.
    ///
    /// Every failure path after `CreateMultipartUpload` must call this:
    /// S3 keeps the parts already uploaded until the multipart upload is
    /// aborted or completed, and they are billed the whole time with no
    /// object visible in the bucket listing. A >64 MiB bundle can leave
    /// gigabytes behind.
    ///
    /// The abort itself is best-effort — the caller is already returning
    /// the real error — but a failed abort means orphaned parts, so it
    /// warns rather than discarding the result silently.
    async fn abort_multipart(&self, bucket: &str, key: &str, upload_id: &str) {
        if let Err(e) = self
            .s3
            .abort_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
        {
            tracing::warn!(
                target: "ebman::aws",
                bucket, key, upload_id,
                error = %e,
                "AbortMultipartUpload failed — uploaded parts may be left billed"
            );
        }
    }

    /// Upload an application bundle from disk to S3. Bundles below
    /// [`MULTIPART_THRESHOLD`] use a single streaming `PutObject` so RAM
    /// stays flat regardless of file size; larger bundles use multipart
    /// upload in [`MULTIPART_PART_SIZE`] chunks, lifting the single-call
    /// 5 GiB ceiling and bounding peak RAM at one part. On any failure
    /// during a multipart upload we issue `AbortMultipartUpload` so S3
    /// reclaims the partial parts rather than billing for orphans.
    pub async fn upload_bundle(
        &self,
        bucket: &str,
        key: &str,
        path: &std::path::Path,
    ) -> Result<()> {
        self.upload_bundle_with(bucket, key, path, MULTIPART_THRESHOLD, MULTIPART_PART_SIZE)
            .await
    }

    /// Same as [`AwsClient::upload_bundle`] but lets the caller pin the threshold
    /// and part size. Intended for tests; production code calls
    /// `upload_bundle` which fixes both at module-level constants.
    pub async fn upload_bundle_with(
        &self,
        bucket: &str,
        key: &str,
        path: &std::path::Path,
        multipart_threshold: u64,
        part_size: u64,
    ) -> Result<()> {
        use aws_sdk_s3::primitives::ByteStream;
        let metadata = tokio::fs::metadata(path)
            .await
            .wrap_err_with(|| format!("stat bundle {}", path.display()))?;
        let size = metadata.len();
        if !should_multipart(size, multipart_threshold) {
            // Single PutObject. `ByteStream::from_path` streams from disk
            // in the SDK's default chunk size — no Vec<u8> of the whole
            // file is allocated.
            let body = ByteStream::from_path(path)
                .await
                .wrap_err_with(|| format!("read {}", path.display()))?;
            self.s3
                .put_object()
                .bucket(bucket)
                .key(key)
                .body(body)
                .send()
                .await
                .wrap_err_with(|| format!("S3 PutObject {bucket}/{key} failed"))?;
            return Ok(());
        }
        // Multipart path.
        let create = self
            .s3
            .create_multipart_upload()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .wrap_err_with(|| format!("S3 CreateMultipartUpload {bucket}/{key} failed"))?;
        let upload_id = create
            .upload_id()
            .ok_or_else(|| eyre!("CreateMultipartUpload returned no UploadId"))?
            .to_string();

        // Per-part upload. We read each chunk into a Vec<u8> sized to
        // the part — RAM = one part, regardless of file size. On any
        // failure mid-loop we abort the upload so S3 doesn't accumulate
        // orphaned parts.
        let plan = plan_part_lengths(size, part_size);
        let mut completed_parts: Vec<aws_sdk_s3::types::CompletedPart> =
            Vec::with_capacity(plan.len());
        // tokio::fs::File implements AsyncReadExt; we read exact chunks.
        use tokio::io::AsyncReadExt;
        let mut file = match tokio::fs::File::open(path).await {
            Ok(f) => f,
            Err(e) => {
                // Best-effort abort — propagate the original open error.
                self.abort_multipart(bucket, key, &upload_id).await;
                return Err(eyre!("open {} for multipart upload: {e}", path.display()));
            }
        };
        for (idx, part_len) in plan.iter().enumerate() {
            let part_number = (idx + 1) as i32;
            let mut buf = vec![0u8; *part_len as usize];
            if let Err(e) = file.read_exact(&mut buf).await {
                self.abort_multipart(bucket, key, &upload_id).await;
                return Err(eyre!(
                    "read part {part_number} from {}: {e}",
                    path.display()
                ));
            }
            let resp = match self
                .s3
                .upload_part()
                .bucket(bucket)
                .key(key)
                .upload_id(&upload_id)
                .part_number(part_number)
                .body(ByteStream::from(buf))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    self.abort_multipart(bucket, key, &upload_id).await;
                    return Err(e).wrap_err_with(|| {
                        format!("S3 UploadPart {part_number} of {bucket}/{key} failed")
                    });
                }
            };
            // S3 omits the ETag in some configurations (SSE-C header
            // mismatch), and mocked or proxied endpoints do it too. This
            // path used to bare-`?` out, skipping the abort that every
            // other failure arm performs and leaving the uploaded parts
            // billed indefinitely.
            let e_tag = match resp.e_tag() {
                Some(t) => t.to_string(),
                None => {
                    self.abort_multipart(bucket, key, &upload_id).await;
                    return Err(eyre!(
                        "S3 UploadPart {part_number} of {bucket}/{key} returned no ETag"
                    ));
                }
            };
            completed_parts.push(
                aws_sdk_s3::types::CompletedPart::builder()
                    .part_number(part_number)
                    .e_tag(e_tag)
                    .build(),
            );
        }

        let completed = aws_sdk_s3::types::CompletedMultipartUpload::builder()
            .set_parts(Some(completed_parts))
            .build();
        if let Err(e) = self
            .s3
            .complete_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(&upload_id)
            .multipart_upload(completed)
            .send()
            .await
        {
            self.abort_multipart(bucket, key, &upload_id).await;
            return Err(e)
                .wrap_err_with(|| format!("S3 CompleteMultipartUpload {bucket}/{key} failed"));
        }
        Ok(())
    }
}
