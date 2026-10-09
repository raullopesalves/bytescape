//! Search results: running a search, paging through it, grouping it by folder.

use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchSummary {
    pub(crate) generation: u64,
    pub(crate) count: usize,
    pub(crate) bytes: u64,
    pub(crate) millis: u64,
    /// The scanned folder with the totals of the matches, for grouping
    /// results by folder.
    pub(crate) root: Option<Row>,
}

/// Runs a search and keeps its results for `search_page` and the treemap.
/// An empty filter clears the search.
#[tauri::command]
pub fn search(state: State<AppState>, filter: search::Filter) -> Result<SearchSummary, String> {
    let mut guard = state.session.lock().unwrap();
    let s = guard.as_mut().ok_or("No scan yet.")?;
    s.generation += 1;
    let generation = s.generation;
    if filter.is_empty() {
        s.search = None;
        return Ok(SearchSummary {
            generation,
            count: 0,
            bytes: 0,
            millis: 0,
            root: None,
        });
    }
    let t = Instant::now();
    let results = search::run(&s.tree, &filter);
    let summary = SearchSummary {
        generation,
        count: results.ids.len(),
        bytes: results.file_bytes,
        millis: t.elapsed().as_millis() as u64,
        root: Some(Row::matched(&s.tree, &results, s.tree.root())),
    };
    s.search = Some(SearchState {
        generation,
        results,
    });
    Ok(summary)
}

#[tauri::command]
pub fn clear_search(state: State<AppState>) {
    if let Some(s) = state.session.lock().unwrap().as_mut() {
        s.generation += 1;
        s.search = None;
    }
}

/// Matches `offset .. offset + limit` of search `generation`, largest first.
#[tauri::command]
pub fn search_page(
    state: State<AppState>,
    generation: u64,
    offset: usize,
    limit: usize,
) -> Result<Vec<Details>, String> {
    let guard = state.session.lock().unwrap();
    let s = guard.as_ref().ok_or("No scan yet.")?;
    let st = s
        .search
        .as_ref()
        .filter(|st| st.generation == generation)
        .ok_or("That search is out of date.")?;
    let ids = st.results.ids.iter().skip(offset).take(limit.min(1000));
    Ok(ids
        .map(|&id| Details {
            row: Row::new(&s.tree, id),
            path: s.tree.path(id).display().to_string(),
        })
        .collect())
}

/// Children of `id` that match search `generation` or hold matches, with
/// their totals counting only the matches.
#[tauri::command]
pub fn search_children(
    state: State<AppState>,
    generation: u64,
    id: NodeId,
) -> Result<Vec<Row>, String> {
    let guard = state.session.lock().unwrap();
    let s = guard.as_ref().ok_or("No scan yet.")?;
    let st = s
        .search
        .as_ref()
        .filter(|st| st.generation == generation)
        .ok_or("That search is out of date.")?;
    if id as usize >= s.tree.len() {
        return Err(format!("No node {id}."));
    }
    Ok(matched_children(&s.tree, &st.results, id)
        .map(|c| Row::matched(&s.tree, &st.results, c))
        .collect())
}

/// The matches of search `generation` at or below `ids`: each node that
/// matches, else the topmost matches below it. Picking a folder in results
/// grouped by folder means its matches, not everything in it.
#[tauri::command]
pub fn search_matches_in(
    state: State<AppState>,
    generation: u64,
    ids: Vec<NodeId>,
) -> Result<Vec<NodeId>, String> {
    let guard = state.session.lock().unwrap();
    let s = guard.as_ref().ok_or("No scan yet.")?;
    let st = s
        .search
        .as_ref()
        .filter(|st| st.generation == generation)
        .ok_or("That search is out of date.")?;
    let (tree, results) = (&s.tree, &st.results);
    let mut stack: Vec<NodeId> = ids
        .into_iter()
        .filter(|&i| (i as usize) < tree.len())
        .collect();
    let mut out = Vec::new();
    while let Some(id) = stack.pop() {
        let mark = results.marks[id as usize];
        if mark & search::SELF != 0 {
            out.push(id);
        } else if mark & search::BELOW != 0 {
            stack.extend(matched_children(tree, results, id));
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Node ids of the results in `ranges` (start, end exclusive) of search
/// `generation`, for acting on many selected results without their details.
#[tauri::command]
pub fn search_ids(
    state: State<AppState>,
    generation: u64,
    ranges: Vec<(usize, usize)>,
) -> Result<Vec<NodeId>, String> {
    let guard = state.session.lock().unwrap();
    let s = guard.as_ref().ok_or("No scan yet.")?;
    let st = s
        .search
        .as_ref()
        .filter(|st| st.generation == generation)
        .ok_or("That search is out of date.")?;
    let ids = &st.results.ids;
    Ok(ranges
        .into_iter()
        .flat_map(|(a, b)| ids.get(a.min(ids.len())..b.min(ids.len())).unwrap_or(&[]))
        .copied()
        .collect())
}
