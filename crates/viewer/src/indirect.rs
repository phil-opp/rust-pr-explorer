//! Calls through items in between: a test that calls a changed function
//! through an unchanged helper is connected to it, on the canvas (through
//! items without a card) and in the call view (through unchanged items).

/// Paths through at most this many items in between are followed. With
/// more, nearly every item would be connected to every other one through
/// common helpers.
pub const MAX_VIA: usize = 2;

/// An item reached through items in between.
#[derive(Debug, PartialEq, Eq)]
pub struct Indirect {
    pub to: String,
    /// The items in between, nearest to the start first: the shortest
    /// paths only, one for each first item.
    pub paths: Vec<Vec<String>>,
}

/// The endpoints that `from` reaches through `next` (callees, or callers)
/// along paths of up to [`MAX_VIA`] items that are no endpoints. An
/// endpoint ends a path; `from`'s own neighbors that are endpoints are
/// left out (they are connected directly). In the order they are found:
/// nearer ones first, then in the order of `next`.
pub fn reach(
    from: &str,
    next: impl Fn(&str) -> Vec<String>,
    endpoint: impl Fn(&str) -> bool,
) -> Vec<Indirect> {
    let mut out: Vec<Indirect> = Vec::new();
    // The paths found at earlier levels end there.
    let mut done: Vec<String> = Vec::new();
    let mut paths: Vec<Vec<String>> = next(from)
        .into_iter()
        .filter(|x| x != from && !endpoint(x))
        .map(|x| vec![x])
        .collect();
    for level in 1..=MAX_VIA {
        let mut longer = Vec::new();
        let mut found_here: Vec<String> = Vec::new();
        for p in &paths {
            let last = p.last().map(String::as_str).unwrap_or_default();
            for y in next(last) {
                if y == from || p.contains(&y) {
                    continue;
                }
                if endpoint(&y) {
                    if done.contains(&y) {
                        continue;
                    }
                    let k = match out.iter().position(|i| i.to == y) {
                        Some(k) => k,
                        None => {
                            out.push(Indirect {
                                to: y.clone(),
                                paths: Vec::new(),
                            });
                            found_here.push(y.clone());
                            out.len() - 1
                        }
                    };
                    if !out[k].paths.iter().any(|q| q[0] == p[0]) {
                        out[k].paths.push(p.clone());
                    }
                } else if level < MAX_VIA {
                    let mut q = p.clone();
                    q.push(y);
                    longer.push(q);
                }
            }
        }
        done.extend(found_here);
        paths = longer;
    }
    out
}

/// "a → b" for the items of a path, given their names.
pub fn path_label(names: &[String]) -> String {
    names.join(" → ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn graph(edges: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        edges
            .iter()
            .map(|(a, bs)| (a.to_string(), bs.iter().map(|b| b.to_string()).collect()))
            .collect()
    }

    fn run(
        g: &HashMap<String, Vec<String>>,
        from: &str,
        ends: &[&str],
    ) -> Vec<(String, Vec<Vec<String>>)> {
        reach(
            from,
            |x| g.get(x).cloned().unwrap_or_default(),
            |x| ends.contains(&x),
        )
        .into_iter()
        .map(|i| (i.to, i.paths))
        .collect()
    }

    fn s(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter()
            .map(|p| p.iter().map(|x| x.to_string()).collect())
            .collect()
    }

    /// One and two items in between are followed, three are not; an
    /// endpoint ends a path, and only the shortest paths are kept.
    #[test]
    fn paths_through_up_to_two_items() {
        let g = graph(&[
            ("t", &["h", "k", "a", "deep"]),
            ("h", &["x", "t"]),
            ("k", &["m"]),
            ("m", &["x", "y"]),
            ("a", &["b"]),
            ("deep", &["d1"]),
            ("d1", &["d2"]),
            ("d2", &["z"]),
        ]);
        assert_eq!(
            run(&g, "t", &["x", "y", "z", "a", "b"]),
            vec![
                ("x".to_string(), s(&[&["h"]])),
                ("y".to_string(), s(&[&["k", "m"]])),
            ]
        );
    }

    /// Several first items to one endpoint give one path each.
    #[test]
    fn one_path_per_first_item() {
        let g = graph(&[
            ("t", &["h", "k"]),
            ("h", &["x", "i"]),
            ("i", &["x"]),
            ("k", &["x"]),
        ]);
        assert_eq!(
            run(&g, "t", &["x"]),
            vec![("x".to_string(), s(&[&["h"], &["k"]]))]
        );
    }
}
