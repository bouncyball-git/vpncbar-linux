//! Import Cisco `.pcf` and vpnc `.conf` files — port of the macOS
//! `parseConfigFile`. Obfuscated `.pcf` secrets (`enc_GroupPwd`,
//! `enc_UserPassword`) are decoded with `cisco-decrypt` (shipped by the vpnc
//! package) straight into the Secret Service.

use crate::model::Profile;
use crate::sys::{run, CISCO_DECRYPT};

pub struct ParsedConfig {
    pub profile: Profile,
    pub secret: Option<String>,
    pub password: Option<String>,
}

pub fn parse_config_file(path: &str) -> Option<ParsedConfig> {
    let raw = std::fs::read_to_string(path).ok()?;
    let lines: Vec<String> = raw.replace('\r', "").split('\n').map(str::to_string).collect();

    let pcf = |key: &str| -> Option<String> {
        let p = format!("{}=", key.to_lowercase());
        lines
            .iter()
            .find(|l| l.to_lowercase().starts_with(&p))
            .map(|l| l[p.len()..].to_string())
    };
    let conf = |key: &str| -> Option<String> {
        let p = format!("{} ", key.to_lowercase());
        lines
            .iter()
            .find(|l| l.to_lowercase().starts_with(&p))
            .map(|l| l[key.len()..].trim().to_string())
    };
    let decrypt = |s: &str| -> Option<String> {
        let r = run(CISCO_DECRYPT, &[s], None);
        if r.ok() {
            let v = r.out.trim().to_string();
            (!v.is_empty()).then_some(v)
        } else {
            None
        }
    };
    let blank = |s: &Option<String>| s.as_ref().map(|x| x.is_empty()).unwrap_or(true);

    let gateway: String;
    let id: String;
    let username: String;
    let mut secret: Option<String>;
    let mut password: Option<String>;

    let lower_has = |prefix: &str| lines.iter().any(|l| l.to_lowercase().starts_with(prefix));

    if lower_has("ipsec gateway ") {
        gateway = conf("IPSec gateway").unwrap_or_default();
        id = conf("IPSec ID").unwrap_or_default();
        username = conf("Xauth username").unwrap_or_default();
        password = conf("Xauth password");
        secret = conf("IPSec secret");
        if blank(&secret) {
            if let Some(obf) = conf("IPSec obfuscated secret") {
                secret = decrypt(&obf);
            }
        }
    } else if lower_has("host=") {
        gateway = pcf("Host").unwrap_or_default();
        id = pcf("GroupName").unwrap_or_default();
        username = pcf("Username").unwrap_or_default();
        secret = pcf("GroupPwd");
        if blank(&secret) {
            if let Some(enc) = pcf("enc_GroupPwd") {
                secret = decrypt(&enc);
            }
        }
        password = pcf("UserPassword");
        if blank(&password) {
            if let Some(enc) = pcf("enc_UserPassword") {
                password = decrypt(&enc);
            }
        }
    } else {
        return None;
    }

    let base = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "imported".into());
    let safe = base.to_lowercase().replace(' ', "-");

    Some(ParsedConfig {
        profile: Profile {
            name: safe,
            gateway,
            id,
            username,
            ..Default::default()
        },
        secret: if blank(&secret) { None } else { secret },
        password: if blank(&password) { None } else { password },
    })
}
