//! A typed env name — `NAME`, or `NAME@REGION` — resolved against the
//! listed fleet. Under `:region all` the same name can be listed once per
//! region, and a command that types one used to take whichever row came
//! first. A bare name now resolves only when it is unique; `@REGION`
//! picks one. EB env names are letters, digits and hyphens, so `@` can
//! never be part of one.

use crate::aws::Environment;

/// Why a typed env name named no single row.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EnvRefError {
    NotFound { name: String },
    NotInRegion { name: String, region: String },
    Ambiguous { name: String, regions: Vec<String> },
}

impl EnvRefError {
    /// One status-bar line, naming the fix.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::NotFound { name } => format!("no env named '{name}' in the current view"),
            Self::NotInRegion { name, region } => {
                format!("no env named '{name}' in {region} in the current view")
            }
            Self::Ambiguous { name, regions } => format!(
                "'{name}' is listed in {} — name one, e.g. {name}@{}",
                regions.join(" and "),
                regions[0]
            ),
        }
    }
}

/// The one row `input` names. `home` is the region a row with no region
/// of its own belongs to.
pub(crate) fn resolve_env_ref<'a>(
    input: &str,
    envs: &'a [Environment],
    home: &str,
) -> Result<&'a Environment, EnvRefError> {
    let region_of = |e: &Environment| -> String {
        e.region
            .clone()
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| home.to_string())
    };
    let (name, region) = match input.split_once('@') {
        Some((name, region)) => (name, Some(region)),
        None => (input, None),
    };
    let named: Vec<&Environment> = envs.iter().filter(|e| e.name == name).collect();
    if let Some(region) = region {
        return named
            .into_iter()
            .find(|e| region_of(e) == region)
            .ok_or_else(|| EnvRefError::NotInRegion {
                name: name.to_string(),
                region: region.to_string(),
            });
    }
    match named.as_slice() {
        [] => Err(EnvRefError::NotFound {
            name: name.to_string(),
        }),
        [one] => Ok(one),
        many => {
            let mut regions: Vec<String> = many.iter().map(|e| region_of(e)).collect();
            regions.sort();
            regions.dedup();
            Err(EnvRefError::Ambiguous {
                name: name.to_string(),
                regions,
            })
        }
    }
}
