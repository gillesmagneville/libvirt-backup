//! Date de compilation (AAAA-MM-JJ, UTC) exposée au programme par `LB_BUILD_DATE` : elle s'affiche dans
//! « À propos ». `SOURCE_DATE_EPOCH` (compilations reproductibles, dpkg-buildpackage) a priorité sur l'horloge.

use std::time::{SystemTime, UNIX_EPOCH};

/// Jour civil (UTC) d'un nombre de jours depuis le 1er janvier 1970 (algorithme de H. Hinnant).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn main() {
    let seconds = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0));
    let (y, m, d) = civil_from_days(seconds.div_euclid(86_400));
    println!("cargo:rustc-env=LB_BUILD_DATE={y:04}-{m:02}-{d:02}");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-changed=build.rs");
}
