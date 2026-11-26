//! Immutable, policy-checked skill catalogs captured for one step.

use crate::HostSkillsSnapshot;
use crate::aliases::SkillCatalogAliases;
use crate::catalog::SkillAuthority;
use crate::catalog::SkillCatalog;
use crate::catalog::SkillSourceKind;
use crate::catalog::merge_catalogs;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::sync::Arc;

/// Fully prepared before tool construction; retained unchanged by all step consumers.
#[derive(Debug)]
pub(crate) struct SkillsStepState {
    pub(crate) discovery_v2: bool,
    pub(crate) tools_enabled: bool,
    pub(crate) catalog: SkillStepCatalog,
    pub(crate) host_catalog: Option<SkillCatalog>,
    pub(crate) host_snapshot: Option<Arc<HostSkillsSnapshot>>,
    pub(crate) cloud_status: CatalogStatus,
    pub(crate) config: crate::SkillsExtensionConfig,
}

/// Policy-checked, enabled skill routes for one step. Hidden skills remain explicitly readable.
#[derive(Debug)]
pub(crate) struct SkillStepCatalog {
    pub(super) merged: SkillCatalog,
    pub(crate) aliases: SkillCatalogAliases,
    /// None means no Cloud catalog was captured for this step.
    cloud_warnings: Option<Vec<String>>,
    executor_warnings: Vec<String>,
    host_warnings: Vec<String>,
    host_catalog: Option<SkillCatalog>,
}

impl SkillStepCatalog {
    pub(crate) fn new(
        cloud: Option<SkillCatalog>,
        executor: SkillCatalog,
        discovery_v2: bool,
        aliases: SkillCatalogAliases,
    ) -> Self {
        let cloud_skill_count = cloud.as_ref().map(|catalog| catalog.entries.len());
        let executor_skill_count = executor.entries.len();
        let cloud_warnings = cloud.as_ref().map(|catalog| catalog.warnings.clone());
        let executor_warnings = executor.warnings.clone();
        let mut catalog = if discovery_v2 {
            merge_catalogs(cloud, executor)
        } else {
            let mut catalog = cloud.unwrap_or_default();
            catalog.is_partial |= executor.is_partial;
            catalog.entries.extend(executor.entries);
            catalog.warnings.extend(executor.warnings);
            catalog
        };
        catalog.entries.retain_mut(|entry| {
            if !entry.enabled {
                return false;
            }
            entry.sources.retain(|source| source.enabled);
            true
        });
        tracing::debug!(
            discovery_v2,
            ?cloud_skill_count,
            executor_skill_count,
            enabled_skill_count = catalog.entries.len(),
            warning_count = catalog.warnings.len(),
            "prepared step skill catalog"
        );
        Self {
            merged: catalog,
            aliases,
            cloud_warnings,
            executor_warnings,
            host_warnings: Vec::new(),
            host_catalog: None,
        }
    }

    pub(crate) fn with_host_catalog(mut self, host: Option<SkillCatalog>) -> Self {
        let mut host = host;
        if let Some(host_catalog) = &mut host {
            self.host_warnings = host_catalog.warnings.clone();
            host_catalog.entries.retain_mut(|entry| {
                if !entry.enabled {
                    return false;
                }
                entry.sources.retain(|source| source.enabled);
                true
            });
            self.merged.extend(host_catalog.clone());
        }
        self.host_catalog = host;
        self
    }

    /// Selects one authority from the prepared catalog without consulting thread state.
    pub(crate) fn select_from_catalog(
        &self,
        authority: SkillToolAuthoritySelector,
    ) -> SkillCatalog {
        let source = match authority {
            SkillToolAuthoritySelector::Host => self.host_catalog.as_ref(),
            SkillToolAuthoritySelector::Cloud | SkillToolAuthoritySelector::Executor => {
                Some(&self.merged)
            }
        };
        let entries = source
            .into_iter()
            .flat_map(|catalog| catalog.entries.iter())
            .filter(|entry| authority.matches(&entry.authority))
            .cloned()
            .collect();
        let warnings = match authority {
            SkillToolAuthoritySelector::Cloud => self.cloud_warnings.clone().unwrap_or_default(),
            SkillToolAuthoritySelector::Executor => self.executor_warnings.clone(),
            SkillToolAuthoritySelector::Host => self.host_warnings.clone(),
        };
        SkillCatalog {
            is_partial: self.merged.is_partial,
            entries,
            warnings,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SkillToolAuthoritySelector {
    Host,
    Cloud,
    Executor,
}

impl SkillToolAuthoritySelector {
    pub(crate) fn matches(self, authority: &SkillAuthority) -> bool {
        match self {
            Self::Host => authority.kind == SkillSourceKind::Host,
            Self::Cloud => authority.kind == SkillSourceKind::Cloud,
            Self::Executor => authority.kind == SkillSourceKind::Executor,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogStatus {
    Unavailable,
    Disabled,
    Enabled,
}
