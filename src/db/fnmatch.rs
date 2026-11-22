//! Python `fnmatch.fnmatch` on POSIX (case-sensitive): `*` matches anything
//! including `/`, `?` one character, `[seq]` / `[!seq]` sets with ranges; an
//! unclosed `[` is a literal.

#[derive(Debug)]
enum Tok {
    Lit(char),
    Any,
    Star,
    Set(bool, Vec<(char, char)>),
}

fn compile(pat: &str) -> Vec<Tok> {
    let p: Vec<char> = pat.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < p.len() {
        let c = p[i];
        i += 1;
        match c {
            '*' => {
                if !matches!(out.last(), Some(Tok::Star)) {
                    out.push(Tok::Star);
                }
            }
            '?' => out.push(Tok::Any),
            '[' => {
                let mut j = i;
                if j < p.len() && p[j] == '!' {
                    j += 1;
                }
                if j < p.len() && p[j] == ']' {
                    j += 1;
                }
                while j < p.len() && p[j] != ']' {
                    j += 1;
                }
                if j >= p.len() {
                    out.push(Tok::Lit('['));
                    continue;
                }
                let mut k = i;
                let neg = p[k] == '!';
                if neg {
                    k += 1;
                }
                let body = &p[k..j];
                let mut ranges = Vec::new();
                let mut m = 0;
                while m < body.len() {
                    if m + 2 < body.len() && body[m + 1] == '-' {
                        ranges.push((body[m], body[m + 2]));
                        m += 3;
                    } else {
                        ranges.push((body[m], body[m]));
                        m += 1;
                    }
                }
                out.push(Tok::Set(neg, ranges));
                i = j + 1;
            }
            other => out.push(Tok::Lit(other)),
        }
    }
    out
}

fn matches(toks: &[Tok], s: &[char]) -> bool {
    // Iterative glob matching with single-star backtracking.
    let (mut ti, mut si) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        let ok = match toks.get(ti) {
            Some(Tok::Lit(c)) => *c == s[si],
            Some(Tok::Any) => true,
            Some(Tok::Set(neg, r)) => r.iter().any(|(a, b)| *a <= s[si] && s[si] <= *b) != *neg,
            Some(Tok::Star) => {
                star = Some((ti, si));
                ti += 1;
                continue;
            }
            None => false,
        };
        if ok {
            ti += 1;
            si += 1;
        } else if let Some((st, ss)) = star {
            ti = st + 1;
            si = ss + 1;
            star = Some((st, ss + 1));
        } else {
            return false;
        }
    }
    toks[ti..].iter().all(|t| matches!(t, Tok::Star))
}

pub fn fnmatch(name: &str, pat: &str) -> bool {
    let s: Vec<char> = name.chars().collect();
    matches(&compile(pat), &s)
}

#[cfg(test)]
mod tests {
    use super::fnmatch;

    #[test]
    fn python_semantics() {
        assert!(fnmatch("Reaper-projects", "Reaper-projects"));
        assert!(fnmatch("/m/a/Reaper-projects", "*/Reaper-projects"));
        assert!(fnmatch("/m/a/b.mp3", "*.mp3"));
        assert!(fnmatch("/m/a/b.mp3", "/m/*"));
        assert!(!fnmatch("b.mp3", "*.flac"));
        assert!(fnmatch("x1", "x?"));
        assert!(fnmatch("xb", "x[a-c]"));
        assert!(!fnmatch("xb", "x[!a-c]"));
        assert!(fnmatch("x[", "x["));
        assert!(!fnmatch("abc", "ABC"));
        assert!(fnmatch("", "*"));
        assert!(fnmatch("aXbXc", "a*b*c"));
    }
}
