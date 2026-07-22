

use serde::{Deserialize, Serialize};

pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageParams {
    pub limit: u32,
    pub next_token: Option<String>,
}

impl PageParams {
    pub fn from_query(limit: Option<u32>, next_token: Option<String>) -> Self {
        Self {
            limit: limit.unwrap_or(DEFAULT_LIMIT),
            next_token: next_token.filter(|token| !token.is_empty()),
        }
        .with_bounded_limit()
    }

    pub fn with_bounded_limit(mut self) -> Self {
        if self.limit == 0 {
            self.limit = DEFAULT_LIMIT;
        } else if self.limit > MAX_LIMIT {
            self.limit = MAX_LIMIT;
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageError {
    InvalidToken,
    AmbiguousToken,
}

pub fn page_by_id<T, F>(items: &[T], params: &PageParams, id_of: F) -> Result<Page<T>, PageError>
where
    T: Clone,
    F: Fn(&T) -> &str,
{
    let start = match params.next_token.as_deref() {
        None | Some("") => 0,
        Some(token) => {
            let mut found: Option<usize> = None;
            for (idx, item) in items.iter().enumerate() {
                if id_of(item) == token {
                    if found.is_some() {
                        return Err(PageError::AmbiguousToken);
                    }
                    found = Some(idx);
                }
            }
            match found {
                Some(idx) => idx + 1,
                None => return Err(PageError::InvalidToken),
            }
        }
    };

    let limit = params.limit as usize;
    let end = start.saturating_add(limit).min(items.len());
    let page_items = items[start..end].to_vec();
    let next_token = if end < items.len() {
        page_items.last().map(|item| id_of(item).to_string())
    } else {
        None
    };

    Ok(Page {
        items: page_items,
        next_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Item {
        id: &'static str,
    }

    fn ids(items: &[Item]) -> Vec<&str> {
        items.iter().map(|item| item.id).collect()
    }

    fn items(ids: &[&'static str]) -> Vec<Item> {
        ids.iter().map(|id| Item { id }).collect()
    }

    fn page(list: &[Item], limit: u32, next_token: Option<&str>) -> Result<Page<Item>, PageError> {
        page_by_id(
            list,
            &PageParams {
                limit,
                next_token: next_token.map(str::to_string),
            },
            |item| item.id,
        )
    }

    #[test]
    fn with_bounded_limit_caps_oversized_page_size() {
        let params = PageParams {
            limit: 10_000,
            next_token: None,
        }
        .with_bounded_limit();
        assert_eq!(params.limit, MAX_LIMIT);
    }

    #[test]
    fn with_bounded_limit_leaves_max_unchanged() {
        let params = PageParams {
            limit: MAX_LIMIT,
            next_token: None,
        }
        .with_bounded_limit();
        assert_eq!(params.limit, MAX_LIMIT);
    }

    #[test]
    fn from_query_defaults_and_strips_empty_token() {
        let params = PageParams::from_query(None, None);
        assert_eq!(params.limit, DEFAULT_LIMIT);
        assert!(params.next_token.is_none());

        let params = PageParams::from_query(Some(0), Some(String::new()));
        assert_eq!(params.limit, DEFAULT_LIMIT);
        assert!(params.next_token.is_none());

        let params = PageParams::from_query(Some(25), Some("abc".into()));
        assert_eq!(params.limit, 25);
        assert_eq!(params.next_token.as_deref(), Some("abc"));
    }

    #[test]
    fn page_by_id_walks_full_list() {
        let list = items(&["a", "b", "c", "d", "e"]);

        let page1 = page(&list, 2, None).expect("page1");
        assert_eq!(ids(&page1.items), ["a", "b"]);
        assert_eq!(page1.next_token.as_deref(), Some("b"));

        let page2 = page(&list, 2, page1.next_token.as_deref()).expect("page2");
        assert_eq!(ids(&page2.items), ["c", "d"]);
        assert_eq!(page2.next_token.as_deref(), Some("d"));

        let page3 = page(&list, 2, page2.next_token.as_deref()).expect("page3");
        assert_eq!(ids(&page3.items), ["e"]);
        assert!(page3.next_token.is_none());
    }

    #[test]
    fn page_by_id_exact_multiple_has_no_trailing_empty_page_token() {
        let list = items(&["a", "b", "c", "d"]);
        let page1 = page(&list, 2, None).expect("page1");
        assert_eq!(ids(&page1.items), ["a", "b"]);
        assert_eq!(page1.next_token.as_deref(), Some("b"));

        let page2 = page(&list, 2, page1.next_token.as_deref()).expect("page2");
        assert_eq!(ids(&page2.items), ["c", "d"]);
        assert!(page2.next_token.is_none());
    }

    #[test]
    fn page_by_id_empty_list() {
        let list: Vec<Item> = Vec::new();
        let result = page(&list, 10, None).expect("empty");
        assert!(result.items.is_empty());
        assert!(result.next_token.is_none());
    }

    #[test]
    fn page_by_id_limit_larger_than_list() {
        let list = items(&["a", "b"]);
        let result = page(&list, 50, None).expect("all");
        assert_eq!(ids(&result.items), ["a", "b"]);
        assert!(result.next_token.is_none());
    }

    #[test]
    fn page_by_id_token_at_last_item_returns_empty_page() {
        let list = items(&["a", "b", "c"]);
        let result = page(&list, 10, Some("c")).expect("past end");
        assert!(result.items.is_empty());
        assert!(result.next_token.is_none());
    }

    #[test]
    fn page_by_id_empty_token_string_starts_at_beginning() {
        let list = items(&["a", "b"]);
        let result = page(&list, 10, Some("")).expect("empty token");
        assert_eq!(ids(&result.items), ["a", "b"]);
    }

    #[test]
    fn page_by_id_rejects_unknown_token() {
        let list = items(&["a"]);
        assert_eq!(
            page(&list, 10, Some("missing")),
            Err(PageError::InvalidToken)
        );
    }

    #[test]
    fn page_by_id_rejects_ambiguous_duplicate_ids() {
        let list = items(&["a", "b", "b", "c"]);
        // First page would emit next_token "b", which matches two rows.
        let page1 = page(&list, 2, None).expect("page1");
        assert_eq!(ids(&page1.items), ["a", "b"]);
        assert_eq!(page1.next_token.as_deref(), Some("b"));

        assert_eq!(
            page(&list, 2, page1.next_token.as_deref()),
            Err(PageError::AmbiguousToken)
        );
    }
}
