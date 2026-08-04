//! Resolves which SiteAdapter handles a source: pin → sniff → generic.
//! A1 wires only sniff + generic; the pinned preset-registry path lands in A2.

use std::sync::Arc;

use super::registry::PresetRegistry;
use super::{ProbeContext, SiteAdapter};
use tracing::warn;

/// Minimum auto-sniff confidence for a non-generic adapter to be selected.
const SNIFF_THRESHOLD: f32 = 0.5;

#[derive(Clone)]
pub struct AdapterResolver {
    adapters: Vec<Arc<dyn SiteAdapter>>,
    generic: Arc<dyn SiteAdapter>,
    registry: PresetRegistry,
}

// `dyn SiteAdapter` is not `Debug` (the trait doesn't require it), so we can't
// `#[derive(Debug)]`. Hand-roll a log-friendly impl over the adapters' stable
// `id()`s instead — more useful in logs than a derived dump would be anyway.
impl std::fmt::Debug for AdapterResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdapterResolver")
            .field(
                "adapters",
                &self.adapters.iter().map(|a| a.id()).collect::<Vec<_>>(),
            )
            .field("generic", &self.generic.id())
            .field("registry_entries", &self.registry.entries.len())
            .finish()
    }
}

impl AdapterResolver {
    pub fn new(
        adapters: Vec<Arc<dyn SiteAdapter>>,
        generic: Arc<dyn SiteAdapter>,
        registry: PresetRegistry,
    ) -> Self {
        Self {
            adapters,
            generic,
            registry,
        }
    }

    /// Pick the best adapter for `ctx`:
    /// 1. registry host-pin (if that adapter is registered),
    /// 2. highest `detect()` score at or above the threshold,
    /// 3. the generic fallback.
    pub async fn resolve(&self, ctx: &ProbeContext) -> Arc<dyn SiteAdapter> {
        // 1. Registry pin (A2a). Match the seed host; use the pinned adapter iff
        //    it is registered, else warn and fall through.
        if let Some(host) = reqwest::Url::parse(&ctx.seed_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            && let Some(entry) = self.registry.match_host(&host)
        {
            if let Some(a) = self.adapters.iter().find(|a| a.id() == entry.adapter_id) {
                return Arc::clone(a);
            }
            warn!(
                host = %host,
                adapter_id = %entry.adapter_id,
                "preset adapter not registered; falling back to sniff/generic"
            );
        }

        // 2. Auto-sniff.
        let mut best: Option<Arc<dyn SiteAdapter>> = None;
        let mut best_score = SNIFF_THRESHOLD;
        for a in &self.adapters {
            let score = a.detect(ctx).await;
            // At or above the running best wins; on equal scores the LAST adapter
            // in `adapters` wins (deterministic; A2 may impose ordering).
            if score >= best_score {
                best_score = score;
                best = Some(Arc::clone(a));
            }
        }
        // 3. Generic fallback.
        best.unwrap_or_else(|| Arc::clone(&self.generic))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ingestion::adapter::generic_web::GenericWebAdapter;
    use crate::ingestion::adapter::gitbook_shelf::GitbookShelfAdapter;
    use crate::ingestion::adapter::gitbook_static::GitbookStaticAdapter;
    use crate::ingestion::adapter::mediawiki::MediaWikiAdapter;
    use crate::ingestion::adapter::registry::{PresetEntry, PresetRegistry};
    use crate::ingestion::adapter::vitepress_llms::VitepressLlmsAdapter;
    use crate::ingestion::adapter::{CrawlLimits, ExtractedPage, PageRef, SiteAdapter, SourceSpec};

    struct Fake {
        id: &'static str,
        score: f32,
    }

    #[async_trait::async_trait]
    impl SiteAdapter for Fake {
        fn id(&self) -> &'static str {
            self.id
        }
        async fn detect(&self, _ctx: &ProbeContext) -> f32 {
            self.score
        }
        async fn discover(
            &self,
            _s: &SourceSpec,
            _l: &CrawlLimits,
        ) -> anyhow::Result<Vec<PageRef>> {
            Ok(vec![])
        }
        async fn extract(&self, _p: &PageRef) -> anyhow::Result<ExtractedPage> {
            Ok(ExtractedPage {
                url: "".into(),
                title: "".into(),
                markdown: "".into(),
                version_coordinate: None,
            })
        }
    }

    fn ctx() -> ProbeContext {
        ProbeContext {
            seed_url: "http://x/".into(),
            root_html: "<html></html>".into(),
        }
    }

    fn registry_with(pattern: &str, adapter_id: &str) -> PresetRegistry {
        PresetRegistry {
            entries: vec![PresetEntry {
                host_pattern: pattern.to_string(),
                adapter_id: adapter_id.to_string(),
                adapter_config: None,
                auto_ingest: true,
                crawl_limits: None,
            }],
        }
    }

    #[tokio::test]
    async fn falls_back_to_generic_when_no_adapter_clears_threshold() {
        let r = AdapterResolver::new(
            vec![Arc::new(Fake {
                id: "low",
                score: 0.2,
            })],
            Arc::new(GenericWebAdapter::new()),
            PresetRegistry::default(),
        );
        assert_eq!(r.resolve(&ctx()).await.id(), "generic-web");
    }

    #[tokio::test]
    async fn picks_highest_scoring_adapter_above_threshold() {
        let r = AdapterResolver::new(
            vec![
                Arc::new(Fake {
                    id: "a",
                    score: 0.6,
                }),
                Arc::new(Fake {
                    id: "b",
                    score: 0.9,
                }),
            ],
            Arc::new(GenericWebAdapter::new()),
            PresetRegistry::default(),
        );
        assert_eq!(r.resolve(&ctx()).await.id(), "b");
    }

    #[tokio::test]
    async fn score_exactly_at_threshold_is_selected() {
        // The threshold is an inclusive minimum bar: a score of exactly
        // SNIFF_THRESHOLD (0.5) selects the adapter over the generic fallback.
        let r = AdapterResolver::new(
            vec![Arc::new(Fake {
                id: "boundary",
                score: 0.5,
            })],
            Arc::new(GenericWebAdapter::new()),
            PresetRegistry::default(),
        );
        assert_eq!(r.resolve(&ctx()).await.id(), "boundary");
    }

    #[tokio::test]
    async fn registry_pin_selects_registered_adapter() {
        // Host matches a preset whose adapter_id IS in the adapter set → pinned.
        let r = AdapterResolver::new(
            vec![Arc::new(Fake {
                id: "mediawiki",
                score: 0.0,
            })], // score 0 ⇒ sniff would NOT pick it
            Arc::new(GenericWebAdapter::new()),
            registry_with("wiki.example.com", "mediawiki"),
        );
        let ctx = ProbeContext {
            seed_url: "https://wiki.example.com/Page".into(),
            root_html: "".into(),
        };
        assert_eq!(r.resolve(&ctx).await.id(), "mediawiki");
    }

    #[tokio::test]
    async fn registry_pin_to_unregistered_adapter_falls_back() {
        // Host matches a preset, but no adapter with that id is registered → generic.
        let r = AdapterResolver::new(
            vec![],
            Arc::new(GenericWebAdapter::new()),
            registry_with("wiki.example.com", "mediawiki"),
        );
        let ctx = ProbeContext {
            seed_url: "https://wiki.example.com/Page".into(),
            root_html: "".into(),
        };
        assert_eq!(r.resolve(&ctx).await.id(), "generic-web");
    }

    #[tokio::test]
    async fn no_registry_match_uses_sniff_then_generic() {
        // Host not in registry → existing sniff (here below threshold) → generic.
        let r = AdapterResolver::new(
            vec![Arc::new(Fake {
                id: "low",
                score: 0.2,
            })],
            Arc::new(GenericWebAdapter::new()),
            registry_with("other.example.com", "mediawiki"),
        );
        let ctx = ProbeContext {
            seed_url: "https://nomatch.example.com/".into(),
            root_html: "".into(),
        };
        assert_eq!(r.resolve(&ctx).await.id(), "generic-web");
    }

    #[tokio::test]
    async fn no_registry_match_sniff_wins_over_generic() {
        let r = AdapterResolver::new(
            vec![Arc::new(Fake {
                id: "sniffed",
                score: 0.9,
            })],
            Arc::new(GenericWebAdapter::new()),
            PresetRegistry::default(),
        );
        let ctx = ProbeContext {
            seed_url: "https://nomatch.example.com/".into(),
            root_html: "".into(),
        };
        assert_eq!(r.resolve(&ctx).await.id(), "sniffed");
    }

    #[tokio::test]
    async fn registry_pins_exact_host_to_registered_vitepress() {
        // A preset pinning an exact host to vitepress-llms resolves to that
        // adapter without sniffing (empty root_html would never score).
        let r = AdapterResolver::new(
            vec![Arc::new(VitepressLlmsAdapter::new())],
            Arc::new(GenericWebAdapter::new()),
            registry_with("docs.example.com", "vitepress-llms"),
        );
        let ctx = ProbeContext {
            seed_url: "https://docs.example.com/".into(),
            root_html: "".into(),
        };
        assert_eq!(r.resolve(&ctx).await.id(), "vitepress-llms");
    }

    #[tokio::test]
    async fn registry_pins_glob_host_to_registered_gitbook_static() {
        // A "*.suffix" glob preset matches any subdomain seed.
        let r = AdapterResolver::new(
            vec![Arc::new(GitbookStaticAdapter::new())],
            Arc::new(GenericWebAdapter::new()),
            registry_with("*.pages.example.com", "gitbook-static"),
        );
        let ctx = ProbeContext {
            seed_url: "https://team.pages.example.com/developer-guide/".into(),
            root_html: "".into(),
        };
        assert_eq!(r.resolve(&ctx).await.id(), "gitbook-static");
    }

    #[tokio::test]
    async fn registry_pins_wiki_host_to_registered_mediawiki() {
        let r = AdapterResolver::new(
            vec![Arc::new(MediaWikiAdapter::new())],
            Arc::new(GenericWebAdapter::new()),
            registry_with("wiki.example.com", "mediawiki"),
        );
        let ctx = ProbeContext {
            seed_url: "https://wiki.example.com/index.php/Main_Page".into(),
            root_html: "".into(),
        };
        assert_eq!(r.resolve(&ctx).await.id(), "mediawiki");
    }

    #[tokio::test]
    async fn registry_pins_shelf_host_to_registered_gitbook_shelf() {
        let r = AdapterResolver::new(
            vec![Arc::new(GitbookShelfAdapter::new())],
            Arc::new(GenericWebAdapter::new()),
            registry_with("gitbook.example.com", "gitbook-shelf"),
        );
        let ctx = ProbeContext {
            seed_url: "http://gitbook.example.com/".into(),
            root_html: "".into(),
        };
        assert_eq!(r.resolve(&ctx).await.id(), "gitbook-shelf");
    }
}
