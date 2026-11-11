//! Remote syslog over UDP (remote_host and friends).  C vs Rust.

mod common;
use common::{Case, check, msg};

fn steps() -> Vec<common::Step> {
    vec![
        msg(13, "prog", Some(9), "notice with pid"),
        msg(15, "prog", None, "debug no pid"),
        msg(11, "kernel", None, "pretend kernel"),
        msg(9, "u", None, "alert"),
    ]
}

#[test]
fn remote_formats() {
    for fmt in ["legacy", "legacy_timestamp", "rfc3164", "rfc5424", "nonsense"] {
        let conf = format!(
            "remote_host = \"127.0.0.1\"\nremote_port = \"@PORT@\"\nremote_format = \"{fmt}\"\n\
             All:\n  logdir = \"@T@/all\"\n  remote_log = 1\n"
        );
        check(Case::new(&format!("remote_{fmt}"), &conf).udp().steps(steps()));
    }
}

#[test]
fn remote_selection() {
    // Named hosts, severity filtering, defaults inherited by blocks and
    // removed again, and a block without a logdir (which never sends).
    let conf = r#"
remote_host = "127.0.0.1"
remote_port = "@PORT@"
remote_log = 1
remote_host[second] = "127.0.0.1"
remote_port[second] = "@PORT@"
remote_format[second] = "rfc3164"
remote_severity_level[second] = 3
remote_port[undefined] = "1"
Errors:
  maximum = 3
  minimum = 9
  remote_log[second] = 1
  remote_log[second] = 1
  logdir = "@T@/err"
No default:
  remote_log = 0
  logdir = "@T@/nodefault"
No logdir:
  remote_log[second] = 1
Stamp only:
  stamp_fmt = "%s"
  log_format = "legacy_timestamp"
  logdir = "@T@/stamp"
"#;
    check(Case::new("remote_selection", conf).udp().steps(steps()));
    // log_severity applies to what is sent too.
    let conf = "remote_host = \"127.0.0.1\"\nremote_port = \"@PORT@\"\nAll:\n  log_severity = 1\n  remote_log = 1\n  logdir = \"@T@/all\"\n";
    check(Case::new("remote_severity_text", conf).udp().steps(steps()));
    // An unresolvable host: nothing is sent, nothing is printed.
    let conf = "remote_host = \"no-such-host.invalid\"\nAll:\n  remote_log = 1\n  logdir = \"@T@/all\"\n";
    check(Case::new("remote_unresolvable", conf).udp().steps(steps()));
}
