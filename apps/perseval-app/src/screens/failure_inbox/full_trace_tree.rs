use std::collections::HashSet;

use perseval_service::{SpanRow, SpanTreePageV1};

use crate::controllers::BoundedPageCache;

pub(super) const TREE_PAGE_SIZE: u64 = 500;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct TreePageKey {
    pub parent_span_id: Option<String>,
    pub page: u64,
}

impl TreePageKey {
    pub fn new(parent_span_id: Option<String>, offset: u64) -> Self {
        Self {
            parent_span_id,
            page: offset / TREE_PAGE_SIZE,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum FullTraceListRow {
    Span(Box<SpanRow>),
    LoadMore {
        parent_span_id: Option<String>,
        offset: u64,
        depth: u32,
    },
    Loading {
        depth: u32,
    },
}

#[derive(Debug, Clone)]
pub(super) struct FullTraceTreeModel {
    pages: BoundedPageCache<TreePageKey, SpanTreePageV1>,
    expanded: HashSet<String>,
    loading: HashSet<TreePageKey>,
}

impl FullTraceTreeModel {
    pub fn new(cached_pages: usize) -> Self {
        Self {
            pages: BoundedPageCache::new(cached_pages),
            expanded: HashSet::new(),
            loading: HashSet::new(),
        }
    }

    pub fn clear(&mut self) {
        self.pages.clear();
        self.expanded.clear();
        self.loading.clear();
    }

    pub fn begin_load(&mut self, key: TreePageKey) -> bool {
        !self.pages.contains_key(&key) && self.loading.insert(key)
    }

    pub fn finish_load(&mut self, key: &TreePageKey, page: SpanTreePageV1) {
        self.loading.remove(key);
        self.pages.insert(key.clone(), page);
    }

    pub fn fail_load(&mut self, key: &TreePageKey) {
        self.loading.remove(key);
    }

    pub fn toggle(&mut self, span_id: &str) -> bool {
        if !self.expanded.remove(span_id) {
            self.expanded.insert(span_id.to_owned());
            true
        } else {
            false
        }
    }

    pub fn is_expanded(&self, span_id: &str) -> bool {
        self.expanded.contains(span_id)
    }

    pub fn expand_all_loaded(&mut self) {
        self.expanded.extend(
            self.pages
                .values()
                .flat_map(|page| page.rows.iter())
                .filter(|span| span.has_children)
                .map(|span| span.span_id.clone()),
        );
    }

    pub fn has_page(&self, key: &TreePageKey) -> bool {
        self.pages.contains_key(key)
    }

    pub fn visible_rows(&self) -> Vec<FullTraceListRow> {
        let mut rows = Vec::new();
        let mut visited = HashSet::new();
        self.append_pages(None, 0, &mut visited, &mut rows);
        rows
    }

    fn append_pages(
        &self,
        parent_span_id: Option<&str>,
        depth: u32,
        visited: &mut HashSet<String>,
        rows: &mut Vec<FullTraceListRow>,
    ) {
        let mut page_index = 0;
        let mut loaded = 0_u64;
        loop {
            let key = TreePageKey {
                parent_span_id: parent_span_id.map(str::to_owned),
                page: page_index,
            };
            let Some(page) = self.pages.peek(&key) else {
                if self.loading.contains(&key) {
                    rows.push(FullTraceListRow::Loading { depth });
                }
                break;
            };
            for span in &page.rows {
                if !visited.insert(span.span_id.clone()) {
                    continue;
                }
                let mut span = span.clone();
                span.depth = depth;
                rows.push(FullTraceListRow::Span(Box::new(span.clone())));
                if span.has_children && self.expanded.contains(&span.span_id) {
                    self.append_pages(Some(&span.span_id), depth + 1, visited, rows);
                }
            }
            loaded = loaded.saturating_add(page.rows.len() as u64);
            if loaded >= page.total {
                break;
            }
            page_index += 1;
            let next = TreePageKey {
                parent_span_id: parent_span_id.map(str::to_owned),
                page: page_index,
            };
            if !self.pages.contains_key(&next) {
                if self.loading.contains(&next) {
                    rows.push(FullTraceListRow::Loading { depth });
                } else {
                    rows.push(FullTraceListRow::LoadMore {
                        parent_span_id: parent_span_id.map(str::to_owned),
                        offset: loaded,
                        depth,
                    });
                }
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn row(id: &str, parent: Option<&str>, has_children: bool) -> SpanRow {
        SpanRow {
            logical_trace_id: "trace".into(),
            revision: 1,
            span_id: id.into(),
            parent_span_id: parent.map(str::to_owned),
            name: id.into(),
            category: "agent".into(),
            start_time_unix_nano: 0,
            duration_nano: 1,
            status_code: 0,
            status_message: String::new(),
            depth: 0,
            has_children,
            attributes: BTreeMap::new(),
            payload_refs: BTreeMap::new(),
            events: Vec::new(),
            links: Vec::new(),
        }
    }

    fn page(parent: Option<&str>, rows: Vec<SpanRow>) -> SpanTreePageV1 {
        SpanTreePageV1 {
            parent_span_id: parent.map(str::to_owned),
            offset: 0,
            total: rows.len() as u64,
            rows,
        }
    }

    #[test]
    fn children_are_absent_until_their_real_parent_is_expanded() {
        let mut tree = FullTraceTreeModel::new(8);
        let root_key = TreePageKey::new(None, 0);
        tree.finish_load(&root_key, page(None, vec![row("root", None, true)]));
        let child_key = TreePageKey::new(Some("root".into()), 0);
        tree.finish_load(
            &child_key,
            page(Some("root"), vec![row("child", Some("root"), false)]),
        );

        assert_eq!(tree.visible_rows().len(), 1);
        assert!(tree.toggle("root"));
        let visible = tree.visible_rows();
        assert_eq!(visible.len(), 2);
        assert!(
            matches!(&visible[1], FullTraceListRow::Span(span) if span.span_id == "child" && span.depth == 1)
        );
        assert!(!tree.toggle("root"));
        assert_eq!(tree.visible_rows().len(), 1);
    }

    #[test]
    fn a_partial_page_exposes_an_explicit_load_more_row() {
        let mut tree = FullTraceTreeModel::new(8);
        let key = TreePageKey::new(None, 0);
        tree.finish_load(
            &key,
            SpanTreePageV1 {
                parent_span_id: None,
                offset: 0,
                total: 501,
                rows: vec![row("root", None, false)],
            },
        );
        assert!(matches!(
            tree.visible_rows().last(),
            Some(FullTraceListRow::LoadMore { offset: 1, .. })
        ));
    }
}
