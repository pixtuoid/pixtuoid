//! Upstream over the network, through the platform's own `curl` and `tar`,
//! which bring no TLS stack or archive crate into the lockfile.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::{API_URL, Fetch, FetchError, RAW_URL};

/// A status that means OUR pin is gone, so the watch is dark for that document:
/// probe health, never transient. Every other failure may pass on a retry.
const PERMANENT_HTTP_STATUS: [u16; 3] = [404, 410, 451];

/// The status rides stderr's last line, so the body stays whole on stdout
/// and a curl error message lands above it; [`classify`] reads it there.
const WRITE_OUT: &str = "%{stderr}\n%{http_code}";

/// The token upstream-drift.yml hands the run, for GitHub's rate limit.
const TOKEN_ENV: &str = "GITHUB_TOKEN";

#[derive(Debug)]
pub(super) struct Platform;

impl Fetch for Platform {
    fn get(&self, url: &str, timeout: Duration) -> Result<Vec<u8>, FetchError> {
        let header = authorized(url)
            .then(|| pixtuoid_core::platform::text_env(TOKEN_ENV))
            .flatten()
            .filter(|token| !token.is_empty())
            .map(|token| format!("Authorization: Bearer {token}\n"));
        let mut child = Command::new("curl")
            .args(curl_args(url, timeout, header.is_some()))
            .stdin(if header.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| FetchError::Transient(format!("could not run curl: {e}")))?;
        if let (Some(header), Some(mut stdin)) = (header, child.stdin.take())
            && let Err(e) = stdin.write_all(header.as_bytes())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(FetchError::Transient(format!(
                "could not hand curl the token: {e}"
            )));
        }
        let out = child
            .wait_with_output()
            .map_err(|e| FetchError::Transient(format!("curl failed: {e}")))?;
        classify(
            out.status.success(),
            &String::from_utf8_lossy(&out.stderr),
            out.stdout,
        )
    }

    fn untar(&self, tarball: &[u8], member: &str) -> Result<Option<Vec<u8>>, FetchError> {
        // Listed first: tar fails alike on a missing member and a broken
        // archive, and only the first is our pin going dark.
        let listing = tar(&["-tzf", "-"], tarball)?;
        if !String::from_utf8_lossy(&listing)
            .lines()
            .any(|l| l == member)
        {
            return Ok(None);
        }
        tar(&["-xzOf", "-", member], tarball).map(Some)
    }
}

/// Whether `url` is GitHub's API or raw content, the only hosts the token is for.
fn authorized(url: &str) -> bool {
    [API_URL, RAW_URL].iter().any(|base| {
        url.strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// curl's arguments. A token rides stdin (`--header @-`), never the argument
/// list, which every local process can read; and a redirect to another origin
/// drops it, since `--location` is not `--location-trusted`
/// (curl.se/docs/manpage.html#-H).
fn curl_args(url: &str, timeout: Duration, token_on_stdin: bool) -> Vec<String> {
    let mut args: Vec<String> = ["--silent", "--show-error", "--location", "--max-time"]
        .map(str::to_owned)
        .into();
    args.push(timeout.as_secs().to_string());
    // A CDN can serve its cached gzip to a request that never asked for one, and
    // curl decodes a body only under `--compressed`
    // (curl.se/docs/manpage.html#--compressed).
    args.push("--compressed".to_owned());
    args.extend(
        [
            "--user-agent",
            "pixtuoid-drift-watch",
            "--write-out",
            WRITE_OUT,
        ]
        .map(str::to_owned),
    );
    if token_on_stdin {
        args.extend(["--header", "@-"].map(str::to_owned));
    }
    args.push(url.to_owned());
    args
}

/// `tar <args>` over `tarball` on stdin: its stdout, or why it failed.
fn tar(args: &[&str], tarball: &[u8]) -> Result<Vec<u8>, FetchError> {
    let mut child = Command::new("tar")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| FetchError::Transient(format!("could not run tar: {e}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| FetchError::Transient("tar has no stdin".to_owned()))?;
    // Written from a thread: tar's stdout fills while it still reads, so one
    // thread doing both deadlocks on a tarball larger than the pipes.
    let out = std::thread::scope(|s| {
        s.spawn(move || stdin.write_all(tarball));
        child.wait_with_output()
    })
    .map_err(|e| FetchError::Transient(format!("tar failed: {e}")))?;
    if !out.status.success() {
        return Err(FetchError::Transient(format!(
            "tar failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// What a curl run means: `ran` is its exit, `stderr` ends in the status.
fn classify(ran: bool, stderr: &str, body: Vec<u8>) -> Result<Vec<u8>, FetchError> {
    let stderr = stderr.trim_end();
    let (above, last) = stderr.rsplit_once('\n').unwrap_or(("", stderr));
    // A curl killed before `--write-out` ran left no status line: then all
    // of stderr is curl's say.
    let (said, status) = match last.trim().parse::<u16>() {
        Ok(s) => (above, s),
        Err(_) => (stderr, 0),
    };
    if !ran || status == 0 {
        return Err(FetchError::Transient(format!(
            "curl failed: {}",
            said.lines().collect::<Vec<_>>().join(" ").trim()
        )));
    }
    match status {
        200..=299 => Ok(body),
        s if PERMANENT_HTTP_STATUS.contains(&s) => Err(FetchError::Gone {
            why: format!("HTTP {s}"),
        }),
        s => Err(FetchError::Transient(format!("HTTP {s}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A permanent status is our pin gone, never transient: bucketing a 404 as
    /// transient kept the weekly job green while it watched nothing.
    #[test]
    fn a_permanent_status_darkens_the_pin_and_the_rest_may_retry() {
        assert_eq!(classify(true, "\n200", b"ok".to_vec()), Ok(b"ok".to_vec()));
        for s in [404, 410, 451] {
            assert_eq!(
                classify(true, &format!("\n{s}"), vec![]),
                Err(FetchError::Gone {
                    why: format!("HTTP {s}")
                })
            );
        }
        for s in [403, 429, 500, 502, 503] {
            assert!(matches!(
                classify(true, &format!("\n{s}"), vec![]),
                Err(FetchError::Transient(_))
            ));
        }
        // A connection that never got a response: curl exits non-zero, `000`.
        assert!(matches!(
            classify(false, "curl: (6) Could not resolve host\n000", vec![]),
            Err(FetchError::Transient(e)) if e.contains("Could not resolve host") && !e.contains("000")
        ));
        assert!(matches!(
            classify(true, "no status line", vec![]),
            Err(FetchError::Transient(e)) if e.contains("no status line")
        ));
        // A body cut off after a 200 line (`--max-time` mid-transfer) is no document.
        assert!(matches!(
            classify(
                false,
                "curl: (28) Operation timed out\n200",
                b"half".to_vec()
            ),
            Err(FetchError::Transient(_))
        ));
        assert_eq!(classify(true, "\n299", vec![]), Ok(vec![]));
        assert!(matches!(
            classify(true, "\n300", vec![]),
            Err(FetchError::Transient(_))
        ));
    }

    /// The listing tells our pin going dark (`None`) from an archive tar
    /// cannot read, and a tarball larger than the pipes extracts whole.
    #[cfg(unix)]
    #[test]
    fn untar_tells_a_missing_member_from_a_broken_archive() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // Incompressible, so the tarball itself outgrows a pipe's buffer.
        let mut x = 1u32;
        let big: Vec<u8> = (0..1 << 20)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                x.to_be_bytes()[0]
            })
            .collect();
        std::fs::create_dir(dir.path().join("pkg")).expect("mkdir");
        std::fs::write(dir.path().join("pkg/schema.json"), &big).expect("write");
        let at = dir.path().to_str().expect("a UTF-8 tempdir");
        let tarball = tar(&["-czf", "-", "-C", at, "pkg"], &[]).expect("tar packs");

        assert!(matches!(
            Platform.untar(&tarball, "pkg/schema.json"),
            Ok(Some(b)) if b == big
        ));
        assert_eq!(Platform.untar(&tarball, "pkg/gone.json"), Ok(None));
        assert!(matches!(
            Platform.untar(b"not a tarball", "pkg/schema.json"),
            Err(FetchError::Transient(_))
        ));
    }

    /// The token goes only to GitHub's API and raw hosts, never to npm, a
    /// closed CLI's page, a release download or a lookalike host, and never
    /// into curl's arguments.
    #[test]
    fn the_token_goes_only_to_github_and_never_into_the_arguments() {
        for url in [
            format!("{API_URL}/o/r"),
            format!("{RAW_URL}/o/r/refs/heads/main/e.ts"),
        ] {
            assert!(authorized(&url), "{url}");
        }
        for url in [
            "https://registry.npmjs.org/reasonix/latest",
            "https://code.claude.com/docs/en/hooks.md",
            "https://github.com/o/r/releases/download/v1.0.0/x.tgz",
            "https://api.github.com/users/o",
            "https://api.github.com/reposx/o/r",
            "https://raw.githubusercontent.com.example/o/r",
            "http://api.github.com/repos/o/r",
        ] {
            assert!(!authorized(url), "{url}");
        }
        let timeout = Duration::from_secs(1);
        let headed = curl_args("u", timeout, true);
        assert!(
            headed.windows(2).any(|w| w == ["--header", "@-"]),
            "{headed:?}"
        );
        assert!(
            !headed.iter().any(|a| a.contains("location-trusted")),
            "{headed:?}"
        );
        let bare = curl_args("u", timeout, false);
        assert!(!bare.iter().any(|a| a == "--header"), "{bare:?}");
        assert_eq!(bare.last().map(String::as_str), Some("u"));
    }

    #[test]
    fn every_fetch_decodes_whatever_encoding_the_server_chose() {
        for token in [true, false] {
            let args = curl_args("u", Duration::from_secs(1), token);
            assert!(args.iter().any(|a| a == "--compressed"), "{args:?}");
        }
    }

    /// What `--write-out` makes curl print is what `classify` reads: curl
    /// expands `%{stderr}` to a switch of stream and `%{http_code}` to the
    /// status (curl.se/docs/manpage.html#-w).
    #[test]
    fn classify_reads_the_status_where_write_out_puts_it() {
        let curl_stderr = |err: &str, code: &str| {
            let (_, after) = WRITE_OUT
                .split_once("%{stderr}")
                .expect("the status goes to stderr");
            format!("{err}{}", after.replace("%{http_code}", code))
        };
        assert_eq!(
            classify(true, &curl_stderr("", "404"), vec![]),
            Err(FetchError::Gone {
                why: "HTTP 404".to_owned()
            })
        );
        assert!(matches!(
            classify(false, &curl_stderr("curl: (6) Could not resolve host", "000"), vec![]),
            Err(FetchError::Transient(e)) if e.contains("Could not resolve host")
        ));
    }
}
