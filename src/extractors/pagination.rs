use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_per_page")]
    pub per_page: u32,
}

/// Pagination envelope shared by list-endpoint DTOs via `#[serde(flatten)]`.
#[derive(Debug, Serialize)]
pub struct PageMeta {
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}

fn default_page() -> u32 {
    1
}

fn default_per_page() -> u32 {
    20
}

impl PaginationParams {
    pub fn offset(&self) -> u32 {
        (self.page.max(1).saturating_sub(1)) * self.limit()
    }

    pub fn limit(&self) -> u32 {
        self.per_page.clamp(1, 100)
    }

    pub fn meta(&self, total: i64) -> PageMeta {
        PageMeta {
            total,
            page: self.page,
            per_page: self.limit(),
        }
    }
}

impl Default for PaginationParams {
    fn default() -> Self {
        Self {
            page: default_page(),
            per_page: default_per_page(),
        }
    }
}
