use anyhow::{Result, bail, ensure};

#[derive(Clone, Copy)]
pub(super) struct Candidate<'a> {
    pub provider: &'a str,
    pub id: &'a str,
    pub name: &'a str,
}

/// Resolve human text, not a stored identity. Access only breaks provider ties;
/// a unique route remains selectable for first-time credential setup.
pub(super) fn resolve<'a>(
    candidates: &[Candidate<'a>],
    text: &str,
    provider: Option<&str>,
    mut usable: impl FnMut(Candidate<'a>) -> Result<bool>,
) -> Result<Candidate<'a>> {
    let text = text.trim();
    ensure!(!text.is_empty(), "model pattern must not be empty");
    let (scope, pattern) = match provider {
        Some(provider) => (Some(provider), text),
        None => match text.split_once('/') {
            Some((prefix, pattern))
                if candidates
                    .iter()
                    .any(|candidate| candidate.provider.eq_ignore_ascii_case(prefix)) =>
            {
                (Some(prefix), pattern)
            }
            _ => (None, text),
        },
    };
    ensure!(!pattern.is_empty(), "model pattern must not be empty");
    let mut pool: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|candidate| {
            scope.is_none_or(|provider| candidate.provider.eq_ignore_ascii_case(provider))
        })
        .collect();
    pool.sort_by_key(|candidate| (candidate.provider, candidate.id));
    pool.dedup_by_key(|candidate| (candidate.provider, candidate.id));

    let exact = |query: &str, folded: bool| {
        pool.iter()
            .copied()
            .filter(|candidate| {
                if folded {
                    candidate.id.to_lowercase() == query.to_lowercase()
                } else {
                    candidate.id == query
                }
            })
            .collect::<Vec<_>>()
    };
    let mut matches = exact(pattern, false);
    if matches.is_empty() {
        matches = exact(pattern, true);
    }
    // A namespaced literal ID can also start with its own provider's name.
    // This never permits a recognized provider prefix to escape its scope.
    if matches.is_empty() && provider.is_none() && scope.is_some() {
        matches = exact(text, false);
        if matches.is_empty() {
            matches = exact(text, true);
        }
    }
    let fuzzy = matches.is_empty();
    if fuzzy {
        let lower = pattern.to_lowercase();
        matches = pool
            .into_iter()
            .filter(|candidate| {
                candidate.id.to_lowercase().contains(&lower)
                    || candidate.name.to_lowercase().contains(&lower)
            })
            .collect();
    }
    ensure!(
        !matches.is_empty(),
        "no known model matches {text:?}{}; run `ion models` to list routes",
        scope.map_or_else(String::new, |provider| format!(
            " within provider {provider:?}"
        ))
    );
    if matches
        .iter()
        .any(|candidate| candidate.provider != matches[0].provider)
    {
        let available = matches
            .iter()
            .copied()
            .filter_map(|candidate| match usable(candidate) {
                Ok(true) => Some(Ok(candidate)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>>>()?;
        if !available.is_empty() {
            matches = available;
        }
    }
    if fuzzy {
        // Pi prefers undated aliases over dated snapshots, then the highest ID.
        let best = matches
            .iter()
            .max_by_key(|candidate| (is_alias(candidate.id), candidate.id))
            .expect("nonempty model matches")
            .id;
        matches.retain(|candidate| candidate.id == best);
    }
    if matches.len() == 1 {
        return Ok(matches[0]);
    }
    let choices = matches
        .iter()
        .take(4)
        .map(|candidate| format!("{:?}", format!("{}/{}", candidate.provider, candidate.id)))
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "ambiguous model pattern {text:?}: {choices}{}; select PROVIDER/MODEL explicitly",
        if matches.len() > 4 { ", …" } else { "" }
    )
}

fn is_alias(id: &str) -> bool {
    !id.rsplit_once('-').is_some_and(|(_, suffix)| {
        suffix.len() == 8 && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(provider: &'static str, id: &'static str) -> Candidate<'static> {
        Candidate {
            provider,
            id,
            name: id,
        }
    }

    #[test]
    fn exact_names_namespaces_and_alias_ranking() {
        let models = [
            candidate("direct", "alpha-20260101"),
            candidate("direct", "alpha-20261001"),
            candidate("direct", "alpha-2"),
            candidate("direct", "alpha-3-latest"),
            candidate("direct", "vendor/alpha:high"),
            candidate("direct", "direct/self:high"),
            Candidate {
                name: "Friendly model",
                ..candidate("direct", "named")
            },
        ];
        for (text, provider, expected) in [
            ("alpha-20260101", None, "alpha-20260101"),
            ("DIRECT/ALPHA-2", None, "alpha-2"),
            ("alpha-", None, "alpha-3-latest"),
            ("alpha-2026", None, "alpha-20261001"),
            ("friendly", None, "named"),
            ("direct/vendor/alpha:high", None, "vendor/alpha:high"),
            ("vendor/alpha:high", Some("direct"), "vendor/alpha:high"),
            ("vendor/alpha:high", None, "vendor/alpha:high"),
            ("direct/self:high", None, "direct/self:high"),
        ] {
            assert_eq!(
                resolve(&models, text, provider, |_| panic!(
                    "single provider needs no credentials"
                ))
                .unwrap()
                .id,
                expected
            );
        }
        assert!(resolve(&models, "alpha:low", None, |_| Ok(true)).is_err());
        assert!(resolve(&models, "alpha*", None, |_| Ok(true)).is_err());
    }

    #[test]
    fn provider_authority_and_access_disambiguation() {
        let models = [
            candidate("direct", "alpha"),
            candidate("gateway", "alpha"),
            candidate("gateway", "direct/other"),
        ];
        assert_eq!(
            resolve(&models, "alpha", None, |candidate| Ok(
                candidate.provider == "gateway"
            ))
            .unwrap()
            .provider,
            "gateway"
        );
        for usable in [false, true] {
            let error = resolve(&models, "alpha", None, |_| Ok(usable))
                .err()
                .unwrap();
            assert!(error.to_string().contains("ambiguous model pattern"));
        }
        assert_eq!(
            resolve(&models, "direct/alpha", None, |_| Ok(false))
                .unwrap()
                .provider,
            "direct"
        );
        assert!(resolve(&models, "direct/other", None, |_| Ok(true)).is_err());
        assert!(resolve(&models, "other", Some("direct"), |_| Ok(true)).is_err());
        assert_eq!(
            resolve(&models, "direct/other", Some("gateway"), |_| Ok(false))
                .unwrap()
                .provider,
            "gateway"
        );
    }

    #[test]
    fn usable_pool_precedes_alias_ranking_without_catalog_order_ties() {
        let models = [
            candidate("locked", "zeta-latest"),
            candidate("open", "zeta-20260101"),
        ];
        assert_eq!(
            resolve(&models, "zeta", None, |candidate| Ok(
                candidate.provider == "open"
            ))
            .unwrap()
            .provider,
            "open"
        );
        assert_eq!(
            resolve(&models, "zeta", None, |_| Ok(false)).unwrap().id,
            "zeta-latest"
        );
        let models = [
            candidate("open", "Alpha"),
            candidate("open", "alpha"),
            candidate("open", "alpha"),
        ];
        assert_eq!(
            resolve(&models, "Alpha", None, |_| Ok(false)).unwrap().id,
            "Alpha"
        );
        assert_eq!(
            resolve(&models, "alpha", None, |_| Ok(false)).unwrap().id,
            "alpha"
        );
        assert!(resolve(&models, "ALPHA", None, |_| Ok(false)).is_err());
    }
}
