//! Finding and installing ICC colour profiles for a monitor.
//!
//! Monitor makers publish their monitor drivers (an .inf plus the .icm colour
//! profile) through the Microsoft Update Catalog, so that's where we look: search
//! by model, fetch the small signed .cab from Microsoft's download servers, unpack
//! it with Windows' own `expand.exe` and pick out the profiles.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const CATALOG: &str = "https://www.catalog.update.microsoft.com";

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogHit {
    pub id: String,
    pub title: String,
    pub products: String,
    pub date: String,
    pub size: String,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(60)))
        .user_agent("HIDConfig (+https://github.com/DDQW/HIDConfig)")
        .build()
        .into()
}

fn cell(row: &str, id: &str, col: u8) -> String {
    let marker = format!("id=\"{id}_C{col}_");
    let Some(start) = row.find(&marker) else {
        return String::new();
    };
    let rest = &row[start..];
    let Some(open) = rest.find('>') else {
        return String::new();
    };
    let end = rest.find("</td>").unwrap_or(rest.len());
    strip_tags(&rest[open + 1..end])
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parses catalog search result rows (`<tr id="{guid}_R{n}">`).
pub fn parse_results(html: &str) -> Vec<CatalogHit> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("<tr id=\"") {
        rest = &rest[pos + 8..];
        let Some(q) = rest.find('"') else { break };
        let rid = &rest[..q];
        let Some((id, _)) = rid.split_once("_R") else {
            continue;
        };
        if id.len() != 36 {
            continue;
        }
        let end = rest.find("</tr>").unwrap_or(rest.len());
        let row = &rest[..end];
        let size = row
            .find(&format!("id=\"{id}_size\">"))
            .map(|p| {
                strip_tags(&row[p..].split("</span>").next().unwrap_or_default()[id.len() + 11..])
            })
            .unwrap_or_default();
        out.push(CatalogHit {
            id: id.to_string(),
            title: cell(row, id, 1),
            products: cell(row, id, 2),
            date: cell(row, id, 4),
            size,
        });
    }
    out
}

/// Searches the Microsoft Update Catalog (driver packages only).
pub fn search(query: &str) -> Result<Vec<CatalogHit>> {
    let url = format!("{CATALOG}/Search.aspx?q={}", urlencode(query));
    let html = agent()
        .get(&url)
        .call()
        .context("catalog search")?
        .body_mut()
        .read_to_string()?;
    Ok(parse_results(&html)
        .into_iter()
        .filter(|h| h.products.contains("Driver") || h.title.contains(" - "))
        .collect())
}

/// Searches with a few spellings of the monitor model and merges the hits.
pub fn search_for_monitor(vendor: &str, model: &str) -> Result<Vec<CatalogHit>> {
    let mut hits: Vec<CatalogHit> = Vec::new();
    let mut queries = vec![format!("{vendor} {model}"), model.to_string()];
    queries.dedup();
    for q in queries.iter().filter(|q| q.trim().len() >= 3) {
        for h in search(q)? {
            if !hits.iter().any(|x| x.id == h.id) {
                hits.push(h);
            }
        }
    }
    Ok(hits)
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            b' ' => "%20".into(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// Download link(s) of a catalog entry.
pub fn download_urls(id: &str) -> Result<Vec<String>> {
    let body = format!(
        "updateIDs={}",
        urlencode(&format!(
            r#"[{{"size":0,"languages":"","uidInfo":"{id}","updateID":"{id}"}}]"#
        ))
    );
    let text = agent()
        .post(&format!("{CATALOG}/DownloadDialog.aspx"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(body)
        .context("catalog download dialog")?
        .body_mut()
        .read_to_string()?;
    let urls: Vec<String> = text
        .split(".url = '")
        .skip(1)
        .filter_map(|s| s.split('\'').next())
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .filter(|u| u.contains(".windowsupdate.com/") || u.contains(".microsoft.com/"))
        .map(String::from)
        .collect();
    if urls.is_empty() {
        bail!("the catalog returned no download link");
    }
    Ok(urls)
}

/// Where downloaded packages are unpacked.
pub fn cache_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
        .join("HIDConfig")
        .join("profiles")
}

/// Downloads a catalog entry and returns the ICC profiles inside it.
pub fn fetch_profiles(id: &str) -> Result<Vec<PathBuf>> {
    let dir = cache_dir().join(id);
    std::fs::create_dir_all(&dir)?;
    let mut found = Vec::new();
    for (i, url) in download_urls(id)?.iter().enumerate() {
        let cab = dir.join(format!("package{i}.cab"));
        let mut resp = agent()
            .get(url)
            .call()
            .with_context(|| format!("downloading {url}"))?;
        let bytes = resp
            .body_mut()
            .with_config()
            .limit(50 * 1024 * 1024)
            .read_to_vec()?;
        std::fs::write(&cab, bytes)?;
        let out = dir.join(format!("package{i}"));
        std::fs::create_dir_all(&out)?;
        // Windows' own cabinet extractor.
        let status = std::process::Command::new("expand.exe")
            .arg(&cab)
            .arg("-F:*")
            .arg(&out)
            .stdout(std::process::Stdio::null())
            .status()
            .context("running expand.exe")?;
        if !status.success() {
            bail!("expand.exe could not unpack {}", cab.display());
        }
        found.extend(profiles_in(&out));
    }
    Ok(found)
}

fn profiles_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(profiles_in(&p));
        } else if p
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("icm") || x.eq_ignore_ascii_case("icc"))
        {
            out.push(p);
        }
    }
    out
}

/// Copies a profile into Windows' colour directory (needs administrator rights).
pub fn install(path: &Path) -> Result<String> {
    use windows::{Win32::UI::ColorSystem::InstallColorProfileW, core::PCWSTR};
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain([0])
        .collect();
    // SAFETY: NUL-terminated path; None = local machine.
    unsafe { InstallColorProfileW(None, PCWSTR(wide.as_ptr())) }
        .ok()
        .context("InstallColorProfile (run as administrator)")?;
    Ok(path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default())
}

/// Opens a web search for the monitor's colour profile / driver in the browser.
pub fn web_search(vendor: &str, model: &str) -> Result<()> {
    let url = format!(
        "https://www.bing.com/search?q={}",
        urlencode(&format!("{vendor} {model} monitor driver icc profile"))
    );
    let wide: Vec<u16> = url.encode_utf16().chain([0]).collect();
    // SAFETY: NUL-terminated URL; ShellExecute opens it in the default browser.
    let r = unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            windows::core::w!("open"),
            windows::core::PCWSTR(wide.as_ptr()),
            None,
            None,
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    if r.0 as usize <= 32 {
        bail!("could not open the browser");
    }
    Ok(())
}

/// Plug and Play vendor ids (EDID) -> names used in the catalog.
pub fn vendor_name(pnp: &str) -> &str {
    match pnp {
        "ACI" | "AUS" => "ASUS",
        "ACR" => "Acer",
        "AOC" => "AOC",
        "APP" => "Apple",
        "BNQ" => "BenQ",
        "DEL" => "Dell",
        "EIZ" | "ENC" => "EIZO",
        "GSM" => "LG",
        "HPN" | "HWP" => "HP",
        "IVM" => "iiyama",
        "LEN" => "Lenovo",
        "MSI" => "MSI",
        "NEC" => "NEC",
        "PHL" => "Philips",
        "SAM" => "Samsung",
        "SNY" => "Sony",
        "VSC" => "ViewSonic",
        "GBT" => "Gigabyte",
        "HKC" => "HKC",
        _ => pnp,
    }
}

/// Decodes the EDID manufacturer id (as Windows reports it, byte-swapped) to "IVM" etc.
pub fn pnp_id(edid_manufacturer: u16) -> String {
    let v = edid_manufacturer.swap_bytes();
    let c = |shift: u16| (((v >> shift) & 0x1f) as u8 + b'A' - 1) as char;
    [c(10), c(5), c(0)].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_catalog_rows() {
        let html = r#"<table><tr id="8a3b5789-1dab-481e-833b-6365bfe30f2e_R1" style="">
            <td id="8a3b5789-1dab-481e-833b-6365bfe30f2e_C0_R1"></td>
            <td class="x" id="8a3b5789-1dab-481e-833b-6365bfe30f2e_C1_R1"><a href="javascript:void(0);">
                Iiyama - LCD, Other hardware - iiyama PL2783Q (HDMI)</a></td>
            <td id="8a3b5789-1dab-481e-833b-6365bfe30f2e_C2_R1">Windows 7, Windows 8 Drivers</td>
            <td id="8a3b5789-1dab-481e-833b-6365bfe30f2e_C3_R1">Drivers (Other Hardware)</td>
            <td id="8a3b5789-1dab-481e-833b-6365bfe30f2e_C4_R1">5/4/2015</td>
            <td id="8a3b5789-1dab-481e-833b-6365bfe30f2e_C6_R1"><span id="8a3b5789-1dab-481e-833b-6365bfe30f2e_size">25 KB</span></td>
        </tr></table>"#;
        let hits = parse_results(html);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "8a3b5789-1dab-481e-833b-6365bfe30f2e");
        assert_eq!(
            hits[0].title,
            "Iiyama - LCD, Other hardware - iiyama PL2783Q (HDMI)"
        );
        assert_eq!(hits[0].date, "5/4/2015");
        assert_eq!(hits[0].size, "25 KB");
    }

    #[test]
    fn pnp_ids() {
        // "IVM" packed as 5-bit letters, byte-swapped like DISPLAYCONFIG reports it.
        let packed: u16 = ((b'I' - b'A' + 1) as u16) << 10
            | ((b'V' - b'A' + 1) as u16) << 5
            | (b'M' - b'A' + 1) as u16;
        assert_eq!(pnp_id(packed.swap_bytes()), "IVM");
        assert_eq!(vendor_name("IVM"), "iiyama");
    }
}
