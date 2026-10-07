//! Relay connections (`first`/`last`/`after`/`before`, `pageInfo`,
//! `totalCount`, `edges`/`nodes`) over offset windows.
//!
//! Cursors are opaque base64 strings encoding the 1-based position of an
//! item in the (filtered, ordered) list, so every list can be paginated in
//! both directions with plain `OFFSET`/`LIMIT` or `row_number()` windows.

use async_graphql::{Context, SimpleObject};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::ctx::{GResult, err};

/// Largest page GitHub serves.
pub const MAX_PAGE: i64 = 100;

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ConnArgs {
    pub first: Option<i32>,
    pub last: Option<i32>,
    pub after: Option<String>,
    pub before: Option<String>,
}

impl ConnArgs {
    pub fn new(
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> Self {
        Self {
            first,
            last,
            after,
            before,
        }
    }

    pub fn first(n: i32) -> Self {
        Self {
            first: Some(n),
            ..Default::default()
        }
    }

    /// Whether resolving the window needs the total item count.
    pub fn needs_total(&self) -> bool {
        self.last.is_some() && self.before.is_none()
    }

    /// Resolve the arguments to an offset window. `total` is required when
    /// [`Self::needs_total`].
    pub fn window(&self, total: Option<i64>) -> GResult<Window> {
        for (name, v) in [("first", self.first), ("last", self.last)] {
            if v.is_some_and(|v| v < 0) {
                return Err(err(
                    "INVALID_CURSOR_ARGUMENTS",
                    format!("`{name}` on a connection cannot be less than zero."),
                ));
            }
        }
        let start = match &self.after {
            Some(c) => decode_cursor(c)?,
            None => 0,
        };
        let mut end: Option<i64> = match &self.before {
            Some(c) => Some(decode_cursor(c)? - 1),
            None => total,
        };
        if let Some(f) = self.first {
            let f = i64::from(f).min(MAX_PAGE);
            end = Some(end.map_or(start + f, |e| e.min(start + f)));
        }
        let mut start = start;
        if let Some(l) = self.last {
            let l = i64::from(l).min(MAX_PAGE);
            let e = end.or(total).unwrap_or(start + l);
            start = start.max(e - l);
            end = Some(e);
        }
        let end = end.unwrap_or(start + MAX_PAGE).min(start + MAX_PAGE);
        Ok(Window {
            offset: start,
            limit: (end - start).max(0),
            backward: self.before.is_some(),
        })
    }
}

/// A resolved page window: skip `offset` items, take `limit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Window {
    pub offset: i64,
    pub limit: i64,
    /// Paginating with `before`: there are always items after the page.
    pub backward: bool,
}

impl Window {
    /// Rows to fetch: one extra row tells whether there's a next page.
    pub fn fetch(&self) -> i64 {
        self.limit + 1
    }
}

pub fn encode_cursor(pos: i64) -> String {
    STANDARD.encode(format!("cursor:v2:{pos}"))
}

pub fn decode_cursor(c: &str) -> GResult<i64> {
    STANDARD
        .decode(c)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.strip_prefix("cursor:v2:").and_then(|n| n.parse().ok()))
        .filter(|n: &i64| *n >= 0)
        .ok_or_else(|| {
            err(
                "INVALID_CURSOR_ARGUMENTS",
                format!("`{c}` does not appear to be a valid cursor."),
            )
        })
}

#[derive(Debug, Clone, SimpleObject)]
pub struct PageInfo {
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub start_cursor: Option<String>,
    pub end_cursor: Option<String>,
}

/// One page of items plus what is needed for `pageInfo` / `totalCount`.
pub struct Page<T> {
    pub items: Vec<T>,
    pub offset: i64,
    pub has_next: bool,
    pub total: i64,
}

impl<T> Page<T> {
    /// Build from rows fetched with [`Window::fetch`] (one extra row).
    pub fn from_fetched(mut rows: Vec<T>, w: Window, total: Option<i64>) -> Self {
        let has_extra = rows.len() as i64 > w.limit;
        rows.truncate(w.limit as usize);
        let has_next = has_extra || w.backward;
        let total = total.unwrap_or(w.offset + rows.len() as i64 + i64::from(has_extra));
        Self {
            items: rows,
            offset: w.offset,
            has_next,
            total,
        }
    }

    /// Paginate an in-memory list.
    pub fn from_vec(all: Vec<T>, args: &ConnArgs) -> GResult<Self> {
        let total = all.len() as i64;
        let w = args.window(Some(total))?;
        let items: Vec<T> = all
            .into_iter()
            .skip(w.offset as usize)
            .take(w.limit as usize)
            .collect();
        let has_next = w.offset + (items.len() as i64) < total;
        Ok(Self {
            items,
            offset: w.offset,
            has_next,
            total,
        })
    }

    pub fn empty() -> Self {
        Self {
            items: vec![],
            offset: 0,
            has_next: false,
            total: 0,
        }
    }

    /// Only the total is known (nodes weren't requested).
    pub fn count_only(total: i64) -> Self {
        Self {
            items: vec![],
            offset: 0,
            has_next: total > 0,
            total,
        }
    }

    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            items: self.items.into_iter().map(f).collect(),
            offset: self.offset,
            has_next: self.has_next,
            total: self.total,
        }
    }

    pub fn page_info(&self) -> PageInfo {
        let n = self.items.len() as i64;
        PageInfo {
            has_next_page: self.has_next,
            has_previous_page: self.offset > 0,
            start_cursor: (n > 0).then(|| encode_cursor(self.offset + 1)),
            end_cursor: (n > 0).then(|| encode_cursor(self.offset + n)),
        }
    }
}

/// Which parts of a connection the query selected (avoid counting or
/// loading rows nobody asked for).
pub struct Wants {
    pub nodes: bool,
    pub total: bool,
}

pub fn wants(ctx: &Context<'_>) -> Wants {
    let la = ctx.look_ahead();
    Wants {
        nodes: la.field("nodes").exists()
            || la.field("edges").exists()
            || la.field("pageInfo").exists(),
        total: la.field("totalCount").exists(),
    }
}

/// Declare a GitHub-named connection + edge type for a node type.
macro_rules! connection {
    ($conn:ident, $edge:ident, $node:ty) => {
        #[derive(async_graphql::SimpleObject)]
        pub struct $edge {
            pub cursor: String,
            pub node: $node,
        }

        #[derive(async_graphql::SimpleObject)]
        pub struct $conn {
            pub edges: Vec<$edge>,
            pub nodes: Vec<$node>,
            pub page_info: $crate::conn::PageInfo,
            pub total_count: i32,
        }

        impl From<$crate::conn::Page<$node>> for $conn {
            fn from(page: $crate::conn::Page<$node>) -> Self {
                let page_info = page.page_info();
                let edges = page
                    .items
                    .iter()
                    .enumerate()
                    .map(|(i, n)| $edge {
                        cursor: $crate::conn::encode_cursor(page.offset + i as i64 + 1),
                        node: n.clone(),
                    })
                    .collect();
                Self {
                    edges,
                    nodes: page.items,
                    page_info,
                    total_count: i32::try_from(page.total).unwrap_or(i32::MAX),
                }
            }
        }

        impl $conn {
            pub fn empty() -> Self {
                $crate::conn::Page::<$node>::empty().into()
            }
        }
    };
}
pub(crate) use connection;

#[cfg(test)]
mod tests {
    use super::*;

    fn w(args: ConnArgs, total: Option<i64>) -> (i64, i64) {
        let w = args.window(total).unwrap();
        (w.offset, w.limit)
    }

    #[test]
    fn windows() {
        assert_eq!(w(ConnArgs::first(10), None), (0, 10));
        assert_eq!(w(ConnArgs::first(1000), None), (0, 100));
        let after = ConnArgs {
            first: Some(5),
            after: Some(encode_cursor(10)),
            ..Default::default()
        };
        assert_eq!(w(after, None), (10, 5));
        let last = ConnArgs {
            last: Some(3),
            ..Default::default()
        };
        assert_eq!(w(last, Some(10)), (7, 3));
        let before = ConnArgs {
            last: Some(3),
            before: Some(encode_cursor(5)),
            ..Default::default()
        };
        assert_eq!(w(before, None), (1, 3));
        let short = ConnArgs {
            last: Some(30),
            ..Default::default()
        };
        assert_eq!(w(short, Some(2)), (0, 2));
        assert!(decode_cursor("bogus").is_err());
    }
}
