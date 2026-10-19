use std::cmp::Ordering;
use std::fmt;

/// An Odoo series: `17.0` is (17, 0), `saas-17.2` is (17, 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

impl Version {
    pub fn parse_branch(name: &str) -> Option<Self> {
        if let Some(rest) = name.strip_prefix("saas-") {
            let (maj, min) = rest.split_once('.')?;
            let v = Version {
                major: maj.parse().ok()?,
                minor: min.parse().ok()?,
            };
            return (v.minor > 0).then_some(v);
        }
        let maj = name.strip_suffix(".0")?;
        Some(Version {
            major: maj.parse().ok()?,
            minor: 0,
        })
    }

    pub fn branch(&self) -> String {
        if self.minor == 0 {
            format!("{}.0", self.major)
        } else {
            format!("saas-{}.{}", self.major, self.minor)
        }
    }

    /// The `version_info` master carries right after this version forks.
    /// Master moves from `(17, 5)` to `(18, 1)` when 18.0 forks, and from `(18, 1)` to `(18, 2)`
    /// when saas-18.1 forks.
    pub fn next_master_release(&self) -> (u32, u32) {
        (self.major, self.minor + 1)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor).cmp(&(other.major, other.minor))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.branch())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_orders() {
        let mut v: Vec<_> = [
            "saas-17.2",
            "17.0",
            "16.0",
            "saas-16.4",
            "master",
            "17.0-foo",
            "saas-17.0",
        ]
        .iter()
        .filter_map(|b| Version::parse_branch(b))
        .collect();
        v.sort();
        let names: Vec<_> = v.iter().map(Version::branch).collect();
        assert_eq!(names, ["16.0", "saas-16.4", "17.0", "saas-17.2"]);
    }
}
