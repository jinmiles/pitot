//! Which processes belong to a target: the processes it names and all their descendants.

use serde::Serialize;

/// A running process from the system's process list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
}

/// What to measure. Every process started by the target, directly or not, is measured with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Pid(u32),
    /// Executable names, with or without `.exe`, compared without case.
    Names(Vec<String>),
}

fn stem(name: &str) -> &str {
    let cut = name.len().saturating_sub(4);
    match name.get(cut..) {
        Some(ext) if ext.eq_ignore_ascii_case(".exe") => &name[..cut],
        _ => name,
    }
}

/// Whether two executable names are the same program: compared without case, with or without `.exe`.
pub fn same_exe(a: &str, b: &str) -> bool {
    stem(a).eq_ignore_ascii_case(stem(b))
}

impl Target {
    fn is_root(&self, entry: &ProcessEntry) -> bool {
        match self {
            Self::Pid(pid) => entry.pid == *pid,
            Self::Names(names) => names.iter().any(|n| same_exe(n, &entry.name)),
        }
    }

    /// A short name for output directories.
    pub fn label(&self) -> String {
        match self {
            Self::Pid(pid) => format!("pid-{pid}"),
            Self::Names(names) => names
                .first()
                .map_or("unnamed", |n| stem(n))
                .to_ascii_lowercase(),
        }
    }
}

/// Fills `out` with the PIDs of the target's process trees. `created` returns a process's creation time when it
/// can be read; a child is accepted only if it was created after its parent, which rules out a parent PID that
/// was reused by an unrelated process.
pub fn select(
    entries: &[ProcessEntry],
    target: &Target,
    mut created: impl FnMut(&ProcessEntry) -> Option<u64>,
    out: &mut Vec<u32>,
) {
    let mut members: Vec<(u32, Option<u64>)> = entries
        .iter()
        .filter(|e| target.is_root(e))
        .map(|e| (e.pid, created(e)))
        .collect();
    loop {
        let mut added = false;
        for entry in entries {
            if entry.pid == entry.parent || members.iter().any(|m| m.0 == entry.pid) {
                continue;
            }
            let Some(&(_, parent_created)) = members.iter().find(|m| m.0 == entry.parent) else {
                continue;
            };
            let child_created = created(entry);
            if let (Some(child), Some(parent)) = (child_created, parent_created)
                && child < parent
            {
                continue;
            }
            members.push((entry.pid, child_created));
            added = true;
        }
        if !added {
            break;
        }
    }
    out.clear();
    out.extend(members.iter().map(|m| m.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, parent: u32, name: &str) -> ProcessEntry {
        ProcessEntry {
            pid,
            parent,
            name: name.to_owned(),
        }
    }

    fn system() -> Vec<ProcessEntry> {
        vec![
            entry(0, 0, "[System Process]"),
            entry(4, 0, "System"),
            entry(100, 4, "explorer.exe"),
            entry(200, 100, "Overlay.exe"),
            entry(210, 200, "Overlay.exe"),
            entry(220, 210, "crashpad.exe"),
            entry(300, 100, "other.exe"),
            // Grandchild started later than the scan order suggests.
            entry(230, 220, "helper.exe"),
        ]
    }

    fn pids(target: &Target, created: impl FnMut(&ProcessEntry) -> Option<u64>) -> Vec<u32> {
        let mut out = Vec::new();
        select(&system(), target, created, &mut out);
        out.sort_unstable();
        out
    }

    #[test]
    fn whole_tree_by_name_or_pid() {
        let all = vec![200, 210, 220, 230];
        assert_eq!(pids(&Target::Names(vec!["overlay".into()]), |_| None), all);
        assert_eq!(
            pids(&Target::Names(vec!["OVERLAY.EXE".into()]), |_| None),
            all
        );
        assert_eq!(pids(&Target::Pid(210), |_| None), vec![210, 220, 230]);
        assert!(pids(&Target::Names(vec!["missing".into()]), |_| None).is_empty());
    }

    #[test]
    fn reused_parent_pid_is_not_followed() {
        // 300's parent 100 is not in the tree, but give helper.exe a creation time before its parent's.
        let created = |e: &ProcessEntry| {
            Some(if e.pid == 230 {
                5
            } else {
                10 + u64::from(e.pid)
            })
        };
        assert_eq!(
            pids(&Target::Names(vec!["overlay".into()]), created),
            vec![200, 210, 220]
        );
    }

    #[test]
    fn pitot_and_its_window_are_one_tree() {
        // Pitot's overlay opens its window by starting its own executable again.
        let entries = [
            entry(100, 4, "explorer.exe"),
            entry(500, 100, "pitot.exe"),
            entry(510, 500, "pitot.exe"),
            entry(600, 100, "pitot-bench.exe"),
        ];
        let mut out = Vec::new();
        select(
            &entries,
            &Target::Names(vec!["pitot.exe".into()]),
            |_| None,
            &mut out,
        );
        out.sort_unstable();
        assert_eq!(out, [500, 510]);
    }

    #[test]
    fn labels() {
        assert_eq!(Target::Pid(42).label(), "pid-42");
        assert_eq!(Target::Names(vec!["pitot.exe".into()]).label(), "pitot");
        assert_eq!(
            Target::Names(vec!["Pitot-Overlay.exe".into()]).label(),
            "pitot-overlay"
        );
    }
}
