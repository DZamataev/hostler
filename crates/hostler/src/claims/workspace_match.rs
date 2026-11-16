//! The registered workspace that owns a path, shared by the claims engine and the device service
//! (P3b, L50, P2-15, P2-18).

/// `ws`: `(workspace id, canonical path)`, the paths as the registry stores them
/// (`project::display_path` form). The workspace whose path is the longest component-wise prefix
/// of `canonical`, so a session in a nested worktree maps to that worktree, not to the checkout
/// around it; `None` when none matches.
///
/// `case_insensitive` is the Windows comparison: case is ignored, `\` and `/` both separate
/// components, and a `\\?\` (or `\\?\UNC\`) prefix on either side is read as `display_path`
/// writes it. Otherwise paths compare byte for byte and only `/` separates.
///
/// Fails closed (P3bT9a-3): `None`, so the session blocks every workspace, for a path that is
/// not canonical (a `.`, `..` or empty component) and, on Unix, for one a case-insensitive
/// volume (macOS) could place in a deeper workspace than the exact comparison does.
pub fn owning_workspace<'a>(
    ws: &'a [(String, String)],
    canonical: &str,
    case_insensitive: bool,
) -> Option<&'a str> {
    let exact = deepest(ws, canonical, case_insensitive, false);
    let depth = |m: Option<(usize, &str)>| m.map(|(depth, _)| depth);
    if !case_insensitive && depth(deepest(ws, canonical, false, true)) > depth(exact) {
        return None;
    }
    exact.map(|(_, id)| id)
}

/// The deepest workspace containing `canonical` and its depth in components; `fold` also
/// ignores case (the Unix ambiguity check).
fn deepest<'a>(
    ws: &'a [(String, String)],
    canonical: &str,
    windows: bool,
    fold: bool,
) -> Option<(usize, &'a str)> {
    let compared =
        |path: &str| normalized(path, windows).map(|p| if fold { p.to_lowercase() } else { p });
    let target = compared(canonical)?;
    ws.iter()
        .filter_map(|(id, path)| {
            let path = compared(path)?;
            let depth = path.split('/').filter(|c| !c.is_empty()).count();
            contains(&path, &target).then_some((depth, id.as_str()))
        })
        .max_by_key(|(depth, _)| *depth)
}

/// The path as compared: Windows' long-path prefix removed and every separator `/` (in Windows
/// form), case folded (Windows), and without a trailing separator except at a root. `None` for
/// an empty path, or one with a `.`, `..` or empty component after its root (`/`, `c:/`, or a
/// UNC path's `//`).
fn normalized(path: &str, windows: bool) -> Option<String> {
    let mut path = path.to_owned();
    if windows {
        if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
            path = format!(r"\\{rest}");
        } else if let Some(rest) = path.strip_prefix(r"\\?\") {
            path = rest.to_owned();
        }
        path = path.replace('\\', "/").to_lowercase();
    }
    while path.len() > 1 && path.ends_with('/') && !(windows && is_drive_root(&path)) {
        path.pop();
    }
    let root = if windows && path.starts_with("//") {
        2
    } else if windows && path.as_bytes().get(1) == Some(&b':') {
        // Byte 1 is ASCII, so byte 0 is too: both offsets are character boundaries.
        if path.as_bytes().get(2) == Some(&b'/') {
            3
        } else {
            2
        }
    } else {
        usize::from(path.starts_with('/'))
    };
    let rest = &path[root..];
    let canonical = rest.is_empty()
        || rest
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..");
    (!path.is_empty() && canonical).then_some(path)
}

/// `c:/`: a trailing separator that belongs to the root.
fn is_drive_root(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() == 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/'
}

/// `outer` is `inner` itself or one of its ancestors, component by component.
fn contains(outer: &str, inner: &str) -> bool {
    match inner.strip_prefix(outer) {
        Some("") => true,
        Some(rest) => outer.ends_with('/') || rest.starts_with('/'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(paths: &[(&str, &str)]) -> Vec<(String, String)> {
        paths
            .iter()
            .map(|(id, path)| ((*id).to_owned(), (*path).to_owned()))
            .collect()
    }

    #[test]
    fn nested_worktree_session_maps_to_the_inner_workspace() {
        let ws = registry(&[
            ("outer", "/work/app"),
            ("inner", "/work/app/.worktrees/feature"),
            ("other", "/work/application"),
        ]);
        let owner = |path| owning_workspace(&ws, path, false);
        assert_eq!(owner("/work/app"), Some("outer"));
        assert_eq!(owner("/work/app/"), Some("outer"));
        assert_eq!(owner("/work/app/src/screens"), Some("outer"));
        assert_eq!(owner("/work/app/.worktrees/feature"), Some("inner"));
        assert_eq!(owner("/work/app/.worktrees/feature/e2e"), Some("inner"));
        // Component-wise: a sibling sharing the prefix is not inside.
        assert_eq!(owner("/work/app2"), None);
        assert_eq!(owner("/work/application/x"), Some("other"));
        assert_eq!(owner("/work/app/.worktrees/feature-2"), Some("outer"));
        assert_eq!(owner("/elsewhere"), None);
        assert_eq!(owner(""), None);
        // P3bT9a-3: a path that is not canonical belongs to nobody (it blocks everyone).
        assert_eq!(owner("/work/app/.worktrees/feature/.."), None);
        assert_eq!(owner("/work/app/./src"), None);
        assert_eq!(owner("/work//app"), None);
        assert_eq!(owner("/work/app/.."), None);
        // Inner by case on a case-insensitive volume, outer by bytes: ambiguous, nobody's.
        assert_eq!(owner("/work/app/.Worktrees/feature"), None);
        assert_eq!(owner("/work/app/.Worktrees/feature/e2e"), None);
        // A case difference that picks no deeper workspace keeps the exact answer.
        assert_eq!(owner("/work/app/SRC"), Some("outer"));
        // The registry order does not matter.
        let reversed: Vec<_> = ws.iter().rev().cloned().collect();
        assert_eq!(
            owning_workspace(&reversed, "/work/app/.worktrees/feature/e2e", false),
            Some("inner")
        );
        // Unix compares case and keeps a backslash as a file name character.
        assert_eq!(owner("/Work/App"), None);
        assert_eq!(owner("/work/app\\x"), None);
        // A root workspace contains everything.
        let root = registry(&[("root", "/")]);
        assert_eq!(owning_workspace(&root, "/work", false), Some("root"));
    }

    #[test]
    fn owning_workspace_is_windows_case_insensitive_in_display_form() {
        let ws = registry(&[
            ("app", r"C:\Work\App"),
            ("wt", r"C:\Work\App\.worktrees\Feature"),
            ("share", r"\\server\share\repo"),
        ]);
        let owner = |path| owning_workspace(&ws, path, true);
        assert_eq!(owner(r"c:\work\app"), Some("app"));
        assert_eq!(owner(r"C:\WORK\APP\src"), Some("app"));
        assert_eq!(owner("C:/Work/App/src"), Some("app"));
        assert_eq!(owner(r"\\?\C:\Work\App\src"), Some("app"));
        assert_eq!(owner(r"c:\work\app\.WORKTREES\feature\e2e"), Some("wt"));
        assert_eq!(owner(r"C:\Work\App2"), None);
        assert_eq!(owner(r"\\?\UNC\server\share\repo\x"), Some("share"));
        assert_eq!(owner(r"\\server\share\repo2"), None);
        assert_eq!(owner(r"D:\Work\App"), None);
        // P3bT9a-3: not canonical, after the drive or UNC root.
        assert_eq!(owner(r"C:\Work\App\.worktrees\Feature\.."), None);
        assert_eq!(owner(r"C:\Work\App\.\src"), None);
        assert_eq!(owner(r"C:\Work\\App"), None);
        assert_eq!(owner(r"\\server\share\repo\..\x"), None);
        assert_eq!(owner("c:\u{e9}x"), None);
        // A case difference is no ambiguity on Windows: it is the same path.
        assert_eq!(owner(r"c:\work\app\.WORKTREES\feature"), Some("wt"));
        // A registry row in long-path form matches too.
        let long = registry(&[("app", r"\\?\C:\Work\App")]);
        assert_eq!(owning_workspace(&long, r"C:\Work\App\x", true), Some("app"));
        // A drive root keeps its separator.
        let drive = registry(&[("drive", r"C:\")]);
        assert_eq!(
            owning_workspace(&drive, r"c:\anything", true),
            Some("drive")
        );
        assert_eq!(owning_workspace(&drive, r"d:\anything", true), None);
    }
}
