//! A small, predictable path language for addressing nodes in a config.
//!
//! `clash-verge-rev` lets users mutate the config with JavaScript. That is
//! powerful but brings a JS engine, non-deterministic user code, and a class of
//! bugs the TUI cannot diagnose. This module offers the 95% case declaratively:
//!
//! ```text
//! dns.enable                     -> config["dns"]["enable"]
//! proxies[3].name                -> the 4th proxy's name
//! proxy-groups[name=PROXY].url   -> the url-test URL of the group called PROXY
//! rules[-1]                      -> the last rule
//! ```
//!
//! Every operation is total: a path that does not resolve is reported, never
//! panics, and never creates intermediate nodes by accident unless asked to.

use std::fmt;

use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// One step of a parsed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// A mapping key.
    Key(String),
    /// A list index, resolved against the list length (so `-1` is the last).
    Index(i64),
    /// The first list element whose `key` field equals `value`.
    Selector {
        /// Field to compare.
        key: String,
        /// Value it must equal.
        value: String,
    },
}

/// A parsed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path {
    segments: Vec<Segment>,
    source: String,
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

impl Path {
    /// Parse a path expression.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] when the expression is malformed.
    pub fn parse(input: &str) -> Result<Self> {
        let src = input.trim();
        if src.is_empty() {
            return Err(Error::invalid("path", "path is empty"));
        }
        let mut segments = Vec::new();
        let mut buf = String::new();
        let mut chars = src.chars();

        let flush_key = |buf: &mut String, segments: &mut Vec<Segment>| {
            if !buf.is_empty() {
                segments.push(Segment::Key(std::mem::take(buf)));
            }
        };

        while let Some(ch) = chars.next() {
            match ch {
                '.' => flush_key(&mut buf, &mut segments),
                '[' => {
                    flush_key(&mut buf, &mut segments);
                    let mut inner = String::new();
                    let mut closed = false;
                    for c in chars.by_ref() {
                        if c == ']' {
                            closed = true;
                            break;
                        }
                        inner.push(c);
                    }
                    if !closed {
                        return Err(Error::invalid("path", format!("unclosed `[` in `{src}`")));
                    }
                    let inner = inner.trim();
                    if inner.is_empty() {
                        return Err(Error::invalid("path", format!("empty `[]` in `{src}`")));
                    }
                    if let Ok(n) = inner.parse::<i64>() {
                        segments.push(Segment::Index(n));
                    } else if let Some((k, v)) = inner.split_once('=') {
                        let k = k.trim();
                        if k.is_empty() {
                            return Err(Error::invalid(
                                "path",
                                format!("selector in `{src}` has no field name"),
                            ));
                        }
                        segments.push(Segment::Selector {
                            key: k.to_owned(),
                            value: unquote(v.trim()),
                        });
                    } else {
                        return Err(Error::invalid(
                            "path",
                            format!("`[{inner}]` is neither an index nor a `key=value` selector"),
                        ));
                    }
                }
                _ => buf.push(ch),
            }
        }
        flush_key(&mut buf, &mut segments);
        if segments.is_empty() {
            return Err(Error::invalid("path", format!("`{src}` has no segments")));
        }
        Ok(Self {
            segments,
            source: src.to_owned(),
        })
    }

    /// The parsed steps.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The final step, which determines how `set`/`remove` behave.
    #[must_use]
    pub fn last(&self) -> &Segment {
        self.segments.last().expect("Path always has >= 1 segment")
    }

    /// The steps leading to the container that holds the last step.
    #[must_use]
    pub fn parent(&self) -> &[Segment] {
        &self.segments[..self.segments.len() - 1]
    }
}

fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        s[1..s.len() - 1].to_owned()
    } else {
        s.to_owned()
    }
}

/// Resolve `path` against `root`, returning the addressed value.
#[must_use]
pub fn get<'a>(root: &'a Value, path: &Path) -> Option<&'a Value> {
    let mut cur = root;
    for seg in path.segments() {
        cur = step(cur, seg)?;
    }
    Some(cur)
}

fn step<'a>(cur: &'a Value, seg: &Segment) -> Option<&'a Value> {
    match seg {
        Segment::Key(k) => cur.as_object()?.get(k),
        Segment::Index(i) => {
            let arr = cur.as_array()?;
            let idx = resolve_index(*i, arr.len())?;
            arr.get(idx)
        }
        Segment::Selector { key, value } => {
            let arr = cur.as_array()?;
            arr.iter()
                .find(|e| e.get(key).and_then(Value::as_str) == Some(value.as_str()))
        }
    }
}

fn resolve_index(i: i64, len: usize) -> Option<usize> {
    if i < 0 {
        // `-i` overflows for `i64::MIN`, and a path expression can contain it:
        // `proxies[-9223372036854775808]` parses cleanly and used to panic here.
        // `unsigned_abs` is total for every `i64`.
        let back = usize::try_from(i.unsigned_abs()).ok()?;
        len.checked_sub(back)
    } else {
        let idx = usize::try_from(i).ok()?;
        (idx < len).then_some(idx)
    }
}

/// Set the value addressed by `path`, creating intermediate mappings as needed.
///
/// List elements are never created implicitly: addressing `rules[7]` on a list
/// of three elements is an error rather than silent padding. Use [`push`] to
/// append.
///
/// The operation is **atomic**: the path is fully resolved and checked before
/// anything is written, so a failed `set` leaves `root` byte-identical. This
/// matters because override profiles apply many operations in sequence — a
/// half-applied set would produce a config the user never asked for.
///
/// # Errors
/// [`Error::InvalidValue`] when the path cannot be resolved.
pub fn set(root: &mut Value, path: &Path, new_value: Value) -> Result<()> {
    check_settable(root, path)?;
    if path.segments().len() == 1 {
        return set_here(root, path.last(), new_value);
    }
    let mut cur = root;
    for seg in path.parent() {
        cur = descend_mut(cur, seg, path)?;
    }
    set_here(cur, path.last(), new_value)
}

/// Resolve every *parent* step read-only, reporting whether [`set`] would
/// succeed against the current document.
fn check_settable(root: &Value, path: &Path) -> Result<()> {
    let mut cur: Option<&Value> = Some(root);
    for seg in path.parent() {
        match seg {
            Segment::Key(k) => {
                cur = match cur {
                    // A missing or null parent will be materialised as a mapping.
                    None => None,
                    Some(v) if v.is_null() => None,
                    Some(v) => {
                        let obj = v.as_object().ok_or_else(|| {
                            Error::invalid(
                                "path",
                                format!(
                                    "`{path}`: `{k}` is a key, but the parent is not a mapping"
                                ),
                            )
                        })?;
                        obj.get(k)
                    }
                };
            }
            Segment::Index(i) => {
                let v = cur.ok_or_else(|| {
                    Error::invalid(
                        "path",
                        format!("`{path}`: the list addressed by `[{i}]` does not exist"),
                    )
                })?;
                let arr = v.as_array().ok_or_else(|| {
                    Error::invalid("path", format!("`{path}`: cannot index a non-list"))
                })?;
                let idx = resolve_index(*i, arr.len()).ok_or_else(|| {
                    Error::invalid(
                        "path",
                        format!("`{path}`: index {i} is out of range (len {})", arr.len()),
                    )
                })?;
                cur = arr.get(idx);
            }
            Segment::Selector { key, value } => {
                let v = cur.ok_or_else(|| {
                    Error::invalid(
                        "path",
                        format!("`{path}`: the list addressed by `[{key}={value}]` does not exist"),
                    )
                })?;
                let arr = v.as_array().ok_or_else(|| {
                    Error::invalid("path", format!("`{path}`: cannot select inside a non-list"))
                })?;
                cur = arr
                    .iter()
                    .find(|e| e.get(key.as_str()).and_then(Value::as_str) == Some(value.as_str()));
                if cur.is_none() {
                    return Err(Error::invalid(
                        "path",
                        format!("`{path}`: no element with {key}={value}"),
                    ));
                }
            }
        }
    }

    // The container the final step writes into.
    match path.last() {
        Segment::Key(k) => match cur {
            None => Ok(()),
            Some(v) if v.is_null() => Ok(()),
            Some(v) if v.is_object() => Ok(()),
            Some(_) => Err(Error::invalid(
                "path",
                format!("`{path}`: cannot set key `{k}` on a non-mapping"),
            )),
        },
        Segment::Index(i) => {
            let v = cur.ok_or_else(|| {
                Error::invalid(
                    "path",
                    format!("`{path}`: the list addressed by `[{i}]` does not exist"),
                )
            })?;
            let arr = v.as_array().ok_or_else(|| {
                Error::invalid(
                    "path",
                    format!("`{path}`: `[`..`]` addresses a list, but the target is not one"),
                )
            })?;
            resolve_index(*i, arr.len()).map(|_| ()).ok_or_else(|| {
                Error::invalid(
                    "path",
                    format!("`{path}`: index {i} is out of range (len {})", arr.len()),
                )
            })
        }
        Segment::Selector { key, value } => {
            let v = cur.ok_or_else(|| {
                Error::invalid(
                    "path",
                    format!("`{path}`: the list addressed by `[{key}={value}]` does not exist"),
                )
            })?;
            let arr = v.as_array().ok_or_else(|| {
                Error::invalid("path", format!("`{path}`: cannot select inside a non-list"))
            })?;
            if arr
                .iter()
                .any(|e| e.get(key.as_str()).and_then(Value::as_str) == Some(value.as_str()))
            {
                Ok(())
            } else {
                Err(Error::invalid(
                    "path",
                    format!("`{path}`: no element with {key}={value}"),
                ))
            }
        }
    }
}

fn set_here(container: &mut Value, seg: &Segment, new_value: Value) -> Result<()> {
    match seg {
        Segment::Key(k) => {
            if container.is_null() {
                *container = Value::Object(Map::new());
            }
            let obj = container.as_object_mut().ok_or_else(|| {
                Error::invalid("path", format!("cannot set key `{k}` on a non-mapping"))
            })?;
            obj.insert(k.clone(), new_value);
            Ok(())
        }
        Segment::Index(i) => {
            let arr = container
                .as_array_mut()
                .ok_or_else(|| Error::invalid("path", "cannot index a non-list".to_owned()))?;
            let idx = resolve_index(*i, arr.len()).ok_or_else(|| {
                Error::invalid(
                    "path",
                    format!("index {i} is out of range (len {})", arr.len()),
                )
            })?;
            arr[idx] = new_value;
            Ok(())
        }
        Segment::Selector { key, value } => {
            let arr = container.as_array_mut().ok_or_else(|| {
                Error::invalid("path", "cannot select inside a non-list".to_owned())
            })?;
            let slot = arr
                .iter_mut()
                .find(|e| e.get(key.as_str()).and_then(Value::as_str) == Some(value.as_str()))
                .ok_or_else(|| Error::invalid("path", format!("no element with {key}={value}")))?;
            *slot = new_value;
            Ok(())
        }
    }
}

fn descend_mut<'a>(cur: &'a mut Value, seg: &Segment, path: &Path) -> Result<&'a mut Value> {
    // Materialise missing mappings, but never guess at list structure.
    if cur.is_null() {
        *cur = Value::Object(Map::new());
    }
    match seg {
        Segment::Key(k) => {
            let obj = cur.as_object_mut().ok_or_else(|| {
                Error::invalid("path", format!("`{path}`: `{k}` is not a mapping key"))
            })?;
            Ok(obj.entry(k.clone()).or_insert(Value::Null))
        }
        Segment::Index(i) => {
            let arr = cur.as_array_mut().ok_or_else(|| {
                Error::invalid("path", format!("`{path}`: cannot index a non-list"))
            })?;
            let len = arr.len();
            let idx = resolve_index(*i, len).ok_or_else(|| {
                Error::invalid(
                    "path",
                    format!("`{path}`: index {i} is out of range (len {len})"),
                )
            })?;
            Ok(&mut arr[idx])
        }
        Segment::Selector { key, value } => {
            let arr = cur.as_array_mut().ok_or_else(|| {
                Error::invalid("path", format!("`{path}`: cannot select inside a non-list"))
            })?;
            arr.iter_mut()
                .find(|e| e.get(key.as_str()).and_then(Value::as_str) == Some(value.as_str()))
                .ok_or_else(|| {
                    Error::invalid("path", format!("`{path}`: no element with {key}={value}"))
                })
        }
    }
}

/// Remove the node addressed by `path`.
///
/// Returns the removed value, or `None` when the path already resolved to
/// nothing — deleting something absent is not an error, which keeps `remove`
/// blocks idempotent.
///
/// # Errors
/// [`Error::InvalidValue`] when an intermediate step cannot be traversed.
pub fn remove(root: &mut Value, path: &Path) -> Result<Option<Value>> {
    if path.segments().len() == 1 {
        return Ok(remove_here(root, path.last()));
    }
    let mut cur = root;
    for seg in path.parent() {
        match step_mut(cur, seg) {
            Some(next) => cur = next,
            None => return Ok(None),
        }
    }
    Ok(remove_here(cur, path.last()))
}

fn step_mut<'a>(cur: &'a mut Value, seg: &Segment) -> Option<&'a mut Value> {
    match seg {
        Segment::Key(k) => cur.as_object_mut()?.get_mut(k),
        Segment::Index(i) => {
            let len = cur.as_array()?.len();
            let idx = resolve_index(*i, len)?;
            cur.as_array_mut()?.get_mut(idx)
        }
        Segment::Selector { key, value } => {
            let arr = cur.as_array_mut()?;
            arr.iter_mut()
                .find(|e| e.get(key.as_str()).and_then(Value::as_str) == Some(value.as_str()))
        }
    }
}

fn remove_here(container: &mut Value, seg: &Segment) -> Option<Value> {
    match seg {
        Segment::Key(k) => container.as_object_mut().and_then(|o| o.remove(k)),
        Segment::Index(i) => {
            let arr = container.as_array_mut()?;
            let idx = resolve_index(*i, arr.len())?;
            Some(arr.remove(idx))
        }
        Segment::Selector { key, value } => {
            // Every match goes, not just the first. "Remove the thing called
            // A" is a statement about A, so running it twice has to mean the
            // same as running it once — and with two entries called A the
            // first-match version removed one and then the other, which made
            // an overlay containing it a one-shot. Duplicate names are invalid
            // in a configuration anyway, so the only documents where this
            // differs are ones the validator already rejects.
            let arr = container.as_array_mut()?;
            let mut removed = None;
            let mut kept = Vec::with_capacity(arr.len());
            for element in arr.drain(..) {
                let matches =
                    element.get(key.as_str()).and_then(Value::as_str) == Some(value.as_str());
                if matches {
                    removed.get_or_insert(element);
                } else {
                    kept.push(element);
                }
            }
            *arr = kept;
            removed
        }
    }
}

/// Append to the list addressed by `path`, creating it if absent.
///
/// # Errors
/// [`Error::InvalidValue`] when the target exists but is not a list.
pub fn push(root: &mut Value, path: &Path, item: Value) -> Result<()> {
    // Every check comes before the first write, because `push` both descends
    // and can fail at the leaf: a descent that had already materialised the
    // keys it needed would leave the document changed *and* report failure,
    // which is the worst of both. The leaf has to name a list, so it has to be
    // a key.
    let Segment::Key(leaf) = path.last() else {
        return Err(Error::invalid(
            "path",
            format!("`{path}` does not name a list to push onto"),
        ));
    };
    // A target that exists and is not a list is a mistake, not something to
    // overwrite.
    if let Some(existing) = get(root, path)
        && !existing.is_null()
        && !existing.is_array()
    {
        return Err(Error::invalid("path", format!("`{path}` is not a list")));
    }
    // And the way down has to be walkable. `push` used to discover this while
    // walking, which is why a rejected path could still add keys.
    check_traversable(root, path)?;
    let mut cur = root;
    for seg in path.parent() {
        cur = descend_mut(cur, seg, path)?;
    }
    if cur.is_null() {
        *cur = Value::Object(Map::new());
    }
    let obj = cur
        .as_object_mut()
        .ok_or_else(|| Error::invalid("path", "cannot push onto a non-mapping"))?;
    let entry = obj
        .entry(leaf.clone())
        .or_insert_with(|| Value::Array(Vec::new()));
    if entry.is_null() {
        *entry = Value::Array(Vec::new());
    }
    entry
        .as_array_mut()
        .ok_or_else(|| Error::invalid("path", format!("`{leaf}` is not a list")))?
        .push(item);
    Ok(())
}

/// A null that stands in for a value that a descent would have materialised.
///
/// The read-only walk has to reason about keys that do not exist yet without
/// creating them, and an absent key and a null one behave identically on the
/// way down.
const ABSENT: Value = Value::Null;

/// Walk `path` without writing anything, and report the first step that a
/// descent would refuse.
///
/// Mirrors [`descend_mut`] step for step — a null is materialised as a mapping,
/// a key may be absent, an index must be in range, a selector must match. The
/// two are kept next to each other on purpose: if one learns a new rule, the
/// other has to as well.
fn check_traversable(root: &Value, path: &Path) -> Result<()> {
    let mut cur: &Value = root;
    for seg in path.parent() {
        cur = match seg {
            Segment::Key(k) => {
                if cur.is_null() {
                    // Materialised as a mapping, then `k` is inserted as null.
                    &ABSENT
                } else {
                    cur.as_object()
                        .ok_or_else(|| {
                            Error::invalid("path", format!("`{path}`: `{k}` is not a mapping key"))
                        })?
                        .get(k)
                        .unwrap_or(&ABSENT)
                }
            }
            Segment::Index(i) => {
                let arr = cur.as_array().ok_or_else(|| {
                    Error::invalid("path", format!("`{path}`: cannot index a non-list"))
                })?;
                let len = arr.len();
                let idx = resolve_index(*i, len).ok_or_else(|| {
                    Error::invalid(
                        "path",
                        format!("`{path}`: index {i} is out of range (len {len})"),
                    )
                })?;
                &arr[idx]
            }
            Segment::Selector { key, value } => cur
                .as_array()
                .ok_or_else(|| {
                    Error::invalid("path", format!("`{path}`: cannot select inside a non-list"))
                })?
                .iter()
                .find(|e| e.get(key.as_str()).and_then(Value::as_str) == Some(value.as_str()))
                .ok_or_else(|| {
                    Error::invalid("path", format!("`{path}`: no element with {key}={value}"))
                })?,
        };
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "mode": "rule",
            "dns": { "enable": false, "nameserver": ["1.1.1.1"] },
            "proxies": [
                { "name": "A", "port": 1 },
                { "name": "B", "port": 2 }
            ],
            "rules": ["MATCH,DIRECT"]
        })
    }

    #[test]
    fn parses_key_index_and_selector_paths() {
        assert_eq!(
            Path::parse("dns.enable").unwrap().segments(),
            &[Segment::Key("dns".into()), Segment::Key("enable".into())]
        );
        assert_eq!(
            Path::parse("proxies[3].name").unwrap().segments(),
            &[
                Segment::Key("proxies".into()),
                Segment::Index(3),
                Segment::Key("name".into())
            ]
        );
        assert_eq!(
            Path::parse("proxy-groups[name=PROXY].url")
                .unwrap()
                .segments(),
            &[
                Segment::Key("proxy-groups".into()),
                Segment::Selector {
                    key: "name".into(),
                    value: "PROXY".into()
                },
                Segment::Key("url".into())
            ]
        );
        assert_eq!(
            Path::parse("proxies[name=\"JP 01\"]").unwrap().segments(),
            &[
                Segment::Key("proxies".into()),
                Segment::Selector {
                    key: "name".into(),
                    value: "JP 01".into()
                }
            ]
        );
    }

    #[test]
    fn rejects_malformed_paths() {
        for bad in ["", "   ", "a[", "a[]", "a[b]", "[]"] {
            assert!(Path::parse(bad).is_err(), "`{bad}` should not parse");
        }
    }

    #[test]
    fn reads_through_every_segment_kind() {
        let v = sample();
        assert_eq!(get(&v, &Path::parse("mode").unwrap()), Some(&json!("rule")));
        assert_eq!(
            get(&v, &Path::parse("dns.nameserver[0]").unwrap()),
            Some(&json!("1.1.1.1"))
        );
        assert_eq!(
            get(&v, &Path::parse("proxies[1].name").unwrap()),
            Some(&json!("B"))
        );
        assert_eq!(
            get(&v, &Path::parse("proxies[-1].name").unwrap()),
            Some(&json!("B"))
        );
        assert_eq!(
            get(&v, &Path::parse("proxies[name=A].port").unwrap()),
            Some(&json!(1))
        );
        assert_eq!(
            get(&v, &Path::parse("rules[-1]").unwrap()),
            Some(&json!("MATCH,DIRECT"))
        );
    }

    #[test]
    fn extreme_indices_resolve_without_panicking() {
        // Regression: `i64::MIN` negates to an overflow, and the path parser
        // accepts it, so reading and writing such a path must fail cleanly
        // rather than panic.
        let v = sample();
        for text in [
            "proxies[-9223372036854775808]",
            "proxies[9223372036854775807]",
        ] {
            let p = Path::parse(text).unwrap_or_else(|e| panic!("{text} should parse: {e}"));
            assert!(get(&v, &p).is_none(), "{text} resolved to something");
        }

        let mut w = sample();
        let before = w.clone();
        for text in [
            "proxies[-9223372036854775808]",
            "proxies[9223372036854775807]",
        ] {
            let p = Path::parse(text).unwrap();
            assert!(
                set(&mut w, &p, json!("x")).is_err(),
                "{text} was accepted by set"
            );
            assert!(
                remove(&mut w, &p).unwrap().is_none(),
                "{text} removed something"
            );
        }
        assert_eq!(w, before, "a rejected write must be a complete no-op");
    }

    #[test]
    fn a_signed_zero_or_explicit_plus_is_just_a_number() {
        // `-0` and `+1` are ordinary integers, and treating them as anything
        // else would be surprising.
        let v = sample();
        assert_eq!(
            get(&v, &Path::parse("proxies[-0]").unwrap()),
            Some(&json!({"name": "A", "port": 1}))
        );
        assert_eq!(
            get(&v, &Path::parse("proxies[+1].name").unwrap()),
            Some(&json!("B"))
        );
        assert_eq!(
            get(&v, &Path::parse("proxies[0]").unwrap()),
            get(&v, &Path::parse("proxies[-0]").unwrap())
        );
    }

    #[test]
    fn returns_none_for_missing_and_out_of_range() {
        let v = sample();
        assert!(get(&v, &Path::parse("nope").unwrap()).is_none());
        assert!(get(&v, &Path::parse("proxies[9]").unwrap()).is_none());
        assert!(get(&v, &Path::parse("proxies[name=Z]").unwrap()).is_none());
        assert!(get(&v, &Path::parse("mode.deeper").unwrap()).is_none());
    }

    #[test]
    fn sets_existing_and_creates_intermediate_mappings() {
        let mut v = sample();
        set(&mut v, &Path::parse("dns.enable").unwrap(), json!(true)).unwrap();
        assert_eq!(v["dns"]["enable"], json!(true));

        set(&mut v, &Path::parse("a.b.c").unwrap(), json!(7)).unwrap();
        assert_eq!(v["a"]["b"]["c"], json!(7));

        set(
            &mut v,
            &Path::parse("proxies[name=B].port").unwrap(),
            json!(443),
        )
        .unwrap();
        assert_eq!(v["proxies"][1]["port"], json!(443));
    }

    #[test]
    fn refuses_to_pad_lists_implicitly() {
        let mut v = sample();
        let err = set(&mut v, &Path::parse("rules[7]").unwrap(), json!("x")).unwrap_err();
        assert!(err.to_string().contains("out of range"), "{err}");
        assert_eq!(
            v["rules"].as_array().unwrap().len(),
            1,
            "list must be untouched"
        );
    }

    #[test]
    fn removes_keys_selectors_and_indices() {
        let mut v = sample();
        assert!(
            remove(&mut v, &Path::parse("dns.enable").unwrap())
                .unwrap()
                .is_some()
        );
        assert!(!v["dns"].as_object().unwrap().contains_key("enable"));

        remove(&mut v, &Path::parse("proxies[name=A]").unwrap()).unwrap();
        assert_eq!(v["proxies"][0]["name"], json!("B"));

        remove(&mut v, &Path::parse("proxies[0]").unwrap()).unwrap();
        assert_eq!(v["proxies"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn removing_something_absent_is_a_no_op() {
        let mut v = sample();
        assert!(
            remove(&mut v, &Path::parse("nothing.here").unwrap())
                .unwrap()
                .is_none()
        );
        assert!(
            remove(&mut v, &Path::parse("proxies[99]").unwrap())
                .unwrap()
                .is_none()
        );
        assert!(
            remove(&mut v, &Path::parse("proxies[name=ZZZ]").unwrap())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn push_appends_to_an_existing_list() {
        let mut v = sample();
        push(&mut v, &Path::parse("rules").unwrap(), json!("MATCH,PROXY")).unwrap();
        assert_eq!(v["rules"].as_array().unwrap().len(), 2);
        assert_eq!(v["rules"][1], json!("MATCH,PROXY"));
    }

    #[test]
    fn push_creates_a_missing_list_anywhere_in_the_tree() {
        let mut v = sample();
        // `push` always targets a *list*; the leaf of the path is the list, not
        // the element.
        push(
            &mut v,
            &Path::parse("rule-providers.blog").unwrap(),
            json!({"type": "http"}),
        )
        .unwrap();
        assert_eq!(v["rule-providers"]["blog"][0]["type"], json!("http"));
    }

    #[test]
    fn push_rejects_a_non_list_target_without_mutating() {
        let mut v = sample();
        let before = v.clone();
        assert!(push(&mut v, &Path::parse("mode").unwrap(), json!("x")).is_err());
        assert_eq!(v, before, "a failed push must leave the document untouched");
        assert!(push(&mut v, &Path::parse("dns").unwrap(), json!("x")).is_err());
        assert_eq!(v, before);
    }

    #[test]
    fn set_never_invents_list_elements() {
        let mut v = json!({});
        // `a[0]` asserts that `a` is a list with a first element. It does not
        // exist, so this must fail rather than fabricate one.
        assert!(set(&mut v, &Path::parse("a[0]").unwrap(), json!("x")).is_err());
        assert!(
            !v.as_object().unwrap().contains_key("a"),
            "nothing may be created"
        );
        assert_eq!(v, json!({}), "a failed set must be a complete no-op");

        // `push` is the explicit list-growing operation.
        push(&mut v, &Path::parse("a").unwrap(), json!("x")).unwrap();
        assert_eq!(v["a"], json!(["x"]));

        // Now that the element exists, the index resolves.
        set(&mut v, &Path::parse("a[0]").unwrap(), json!("y")).unwrap();
        assert_eq!(v["a"], json!(["y"]));
    }
}
