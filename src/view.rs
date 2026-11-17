//! View paths (what TC shows) ↔ entry paths (what the pak stores), SPEC §3.2 / §4.2.1.
//!
//! A mount point splits into a hidden base (leading `../`, `./`, `/`) and a
//! visible part. In `Folder` view the visible part is shown as folders above
//! the entries; in `Root` view entries are shown as stored.

use crate::config::MountPointMode;

/// `../../../TheIsle/Content/` → base `../../../`, visible `[TheIsle, Content]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Mount {
    pub base: String,
    pub visible: Vec<String>,
    /// The original ended with `/` (kept when rebuilding).
    pub trailing_slash: bool,
}

impl Mount {
    pub fn parse(mount: &str) -> Self {
        let norm = mount.replace('\\', "/");
        let mut base = String::new();
        let mut rest = norm.as_str();
        loop {
            if let Some(r) = rest.strip_prefix("../") {
                base.push_str("../");
                rest = r;
            } else if let Some(r) = rest.strip_prefix("./") {
                base.push_str("./");
                rest = r;
            } else if let Some(r) = rest.strip_prefix('/') {
                base.push('/');
                rest = r;
            } else {
                break;
            }
        }
        let visible = rest
            .split('/')
            .filter(|s| !s.is_empty() && *s != "." && *s != "..")
            .map(String::from)
            .collect();
        Mount {
            base,
            visible,
            trailing_slash: norm.ends_with('/'),
        }
    }

    /// Mount point string with the visible part cut to its first `k` segments.
    pub fn with_visible_len(&self, k: usize) -> String {
        let mut s = self.base.clone();
        let vis = self.visible[..k].join("/");
        s.push_str(&vis);
        if !vis.is_empty() && self.trailing_slash {
            s.push('/');
        }
        s
    }

    /// `TheIsle\Content` (Folder view prefix).
    pub fn display_prefix(&self) -> String {
        self.visible.join("\\")
    }
}

/// Splits a TC view path (`a\b\c`) into segments.
pub fn segments(view_path: &str) -> Vec<String> {
    view_path
        .split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
}

#[cfg(test)]
#[test]
fn non_ascii_prefix_does_not_panic() {
    let all = vec!["Ünïcode/ä.txt".to_string(), "x".to_string()];
    // byte offsets inside multi-byte chars must not panic
    let t = DeleteTarget::Folder("ü".into());
    assert_eq!(select(&t, &all).len(), 1);
    let t = DeleteTarget::Folder("x/".into());
    assert!(select(&t, &all).is_empty());
    let t = DeleteTarget::Folder("ünïcode/".into());
    assert_eq!(select(&t, &all).len(), 1);
}

/// Number of leading segments `a` and `b` share (case-insensitive).
fn common_len(a: &[String], b: &[String]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| eq_ci(x, y)).count()
}

/// Result of mapping files to add (view paths of the *files*) into a pak.
#[derive(Debug, PartialEq)]
pub struct AddMapping {
    /// Entry path for each target, same order.
    pub entries: Vec<String>,
    /// Some(new mount point) when targets lie outside the visible part
    /// (Folder view): existing entries must be prefixed with `reroot_prefix`.
    pub new_mount: Option<String>,
    /// `Content/` style prefix for existing entries, empty if no change.
    pub reroot_prefix: String,
}

/// Maps the view paths of files to add. Root view: as is. Folder view:
/// strip the visible part; targets outside it widen the mount point to the
/// deepest folder shared by the visible part and all targets' folders.
pub fn map_add(mode: MountPointMode, mount: &Mount, targets: &[Vec<String>]) -> AddMapping {
    if mode == MountPointMode::Root {
        return AddMapping {
            entries: targets.iter().map(|t| t.join("/")).collect(),
            new_mount: None,
            reroot_prefix: String::new(),
        };
    }
    let v = &mount.visible;
    let mut k = v.len();
    for t in targets {
        let dir = &t[..t.len().saturating_sub(1)];
        k = k.min(common_len(v, dir));
    }
    let entries = targets.iter().map(|t| t[k..].join("/")).collect();
    if k == v.len() {
        AddMapping {
            entries,
            new_mount: None,
            reroot_prefix: String::new(),
        }
    } else {
        AddMapping {
            entries,
            new_mount: Some(mount.with_visible_len(k)),
            reroot_prefix: format!("{}/", v[k..].join("/")),
        }
    }
}

/// What a `DeleteList` item selects.
#[derive(Debug, PartialEq)]
pub enum DeleteTarget {
    /// One entry path.
    File(String),
    /// Everything below this entry-path prefix (`dir/`); empty = everything.
    Folder(String),
    /// Outside the pak's visible part: matches nothing.
    Nothing,
}

/// Maps one TC `DeleteList` item (`a\b.txt` or `a\b\*.*`).
pub fn map_delete(mode: MountPointMode, mount: &Mount, item: &str) -> DeleteTarget {
    let (path, folder) = match item.strip_suffix("*.*") {
        Some(p) => (p, true),
        None => (item, false),
    };
    let segs = segments(path);
    let rel: Vec<String> = match mode {
        MountPointMode::Root => segs,
        MountPointMode::Folder => {
            let v = &mount.visible;
            let k = common_len(v, &segs);
            if k == v.len() {
                segs[k..].to_vec()
            } else if folder && k == segs.len() {
                // a mount folder (ancestor of or equal to the visible part)
                return DeleteTarget::Folder(String::new());
            } else {
                return DeleteTarget::Nothing;
            }
        }
    };
    if folder {
        if rel.is_empty() {
            DeleteTarget::Folder(String::new())
        } else {
            DeleteTarget::Folder(format!("{}/", rel.join("/")))
        }
    } else if rel.is_empty() {
        DeleteTarget::Nothing
    } else {
        DeleteTarget::File(rel.join("/"))
    }
}

/// Entry paths of `all` matched by `target` (case-insensitive).
pub fn select<'a>(target: &DeleteTarget, all: &'a [String]) -> Vec<&'a String> {
    match target {
        DeleteTarget::File(p) => all.iter().filter(|e| eq_ci(e, p)).collect(),
        DeleteTarget::Folder(prefix) => {
            let prefix = prefix.to_lowercase();
            all.iter()
                .filter(|e| e.to_lowercase().starts_with(&prefix))
                .collect()
        }
        DeleteTarget::Nothing => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use MountPointMode::*;

    fn segs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_mounts() {
        let m = Mount::parse("../../../TheIsle/Content/");
        assert_eq!(m.base, "../../../");
        assert_eq!(m.visible, ["TheIsle", "Content"]);
        assert_eq!(m.with_visible_len(1), "../../../TheIsle/");
        assert_eq!(m.with_visible_len(0), "../../../");
        let m = Mount::parse("../../../G1R/Config");
        assert_eq!(m.with_visible_len(1), "../../../G1R");
        let m = Mount::parse("/Game/");
        assert_eq!((m.base.as_str(), m.visible.len()), ("/", 1));
        assert!(Mount::parse("../../../").visible.is_empty());
    }

    #[test]
    fn add_inside_and_outside_mount() {
        let m = Mount::parse("../../../TheIsle/Content/");
        // inside
        let r = map_add(
            Folder,
            &m,
            &[segs(&["TheIsle", "Content", "Maps", "a.umap"])],
        );
        assert_eq!(r.entries, ["Maps/a.umap"]);
        assert_eq!(r.new_mount, None);
        // at the deepest mount folder
        let r = map_add(Folder, &m, &[segs(&["theisle", "content", "x.txt"])]);
        assert_eq!(
            (r.entries[0].as_str(), r.new_mount.is_none()),
            ("x.txt", true)
        );
        // beside: TheIsle\Plugins
        let r = map_add(Folder, &m, &[segs(&["TheIsle", "Plugins", "p.uplugin"])]);
        assert_eq!(r.entries, ["Plugins/p.uplugin"]);
        assert_eq!(r.new_mount.as_deref(), Some("../../../TheIsle/"));
        assert_eq!(r.reroot_prefix, "Content/");
        // panel root
        let r = map_add(Folder, &m, &[segs(&["readme.txt"])]);
        assert_eq!(r.entries, ["readme.txt"]);
        assert_eq!(r.new_mount.as_deref(), Some("../../../"));
        assert_eq!(r.reroot_prefix, "TheIsle/Content/");
        // root view: as is
        let r = map_add(Root, &m, &[segs(&["Maps", "a.umap"])]);
        assert_eq!(
            (r.entries[0].as_str(), r.new_mount.is_none()),
            ("Maps/a.umap", true)
        );
    }

    #[test]
    fn delete_mapping_matches_captured_tc_lists() {
        let m = Mount::parse("../../../Engine/");
        let all = segs(&[
            "Shaders/OpenGL/A.glsl",
            "Shaders/OpenGL/B.glsl",
            "Config/x.ini",
        ]);
        // Folder view (captured: "Engine\Shaders\...\*.*")
        let t = map_delete(Folder, &m, "Engine\\Shaders\\OpenGL\\*.*");
        assert_eq!(t, DeleteTarget::Folder("Shaders/OpenGL/".into()));
        assert_eq!(select(&t, &all).len(), 2);
        let t = map_delete(Folder, &m, "Engine\\Config\\x.ini");
        assert_eq!(select(&t, &all), [&all[2]]);
        // the mount folder itself -> everything
        assert_eq!(
            select(&map_delete(Folder, &m, "Engine\\*.*"), &all).len(),
            3
        );
        // Root view (captured: "Shaders\...\file")
        let t = map_delete(Root, &m, "Shaders\\OpenGL\\A.glsl");
        assert_eq!(select(&t, &all), [&all[0]]);
        // outside
        assert_eq!(
            map_delete(Folder, &m, "Other\\x.ini"),
            DeleteTarget::Nothing
        );
        // prefix must end at a folder boundary
        let t = map_delete(Root, &m, "Shader\\*.*");
        assert!(select(&t, &all).is_empty());
    }
}
