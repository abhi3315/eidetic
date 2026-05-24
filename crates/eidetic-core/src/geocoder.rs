//! Offline reverse geocoding from a GeoNames `cities500` dataset.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

const GEONAMES_BASE: &str = "https://download.geonames.org/export/dump";

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub country_code: String,
    pub country_name: String,
    pub admin1: String,
    pub place: String,
    pub distance_m: f32,
}

#[derive(Clone, Debug)]
struct City {
    name: String,
    lat: f64,
    lon: f64,
    country_code: String,
    admin1_code: String,
}

pub struct Geocoder {
    cities: Vec<City>,
    admin1: HashMap<(String, String), String>,
    country: HashMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "GeoNames dataset missing in {dir} (expected cities500.txt, admin1CodesASCII.txt, countryInfo.txt)"
    )]
    DatasetMissing { dir: PathBuf },

    #[error("I/O error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("parse error in {path} (line {line}): {reason}")]
    Parse {
        path: PathBuf,
        line: usize,
        reason: String,
    },

    #[error("download failed for {url}: {source}")]
    Download {
        url: String,
        #[source]
        source: Box<ureq::Error>,
    },

    #[error("zip error in {path}: {source}")]
    Zip {
        path: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },
}

impl Geocoder {
    pub fn open(data_dir: &Path) -> Result<Self, Error> {
        let cities_path = data_dir.join("cities500.txt");
        let admin1_path = data_dir.join("admin1CodesASCII.txt");
        let country_path = data_dir.join("countryInfo.txt");
        if !cities_path.exists() || !admin1_path.exists() || !country_path.exists() {
            return Err(Error::DatasetMissing {
                dir: data_dir.to_path_buf(),
            });
        }

        let country = load_country_info(&country_path)?;
        let admin1 = load_admin1_codes(&admin1_path)?;
        let cities = load_cities500(&cities_path)?;
        if cities.is_empty() {
            return Err(Error::Parse {
                path: cities_path,
                line: 0,
                reason: "no rows parsed".into(),
            });
        }

        Ok(Self {
            cities,
            admin1,
            country,
        })
    }

    /// Download missing GeoNames files into `data_dir`. Writes via `.partial`
    /// then renames so an interrupted run never leaves a torn real file.
    pub fn ensure_dataset(data_dir: &Path) -> Result<(), Error> {
        let cities = data_dir.join("cities500.txt");
        let admin1 = data_dir.join("admin1CodesASCII.txt");
        let country = data_dir.join("countryInfo.txt");
        if cities.exists() && admin1.exists() && country.exists() {
            return Ok(());
        }
        fs::create_dir_all(data_dir).map_err(|source| Error::Io {
            path: data_dir.to_path_buf(),
            source,
        })?;

        if !cities.exists() {
            download_and_extract_cities500(data_dir, &cities)?;
        }
        for (name, dest) in [
            ("admin1CodesASCII.txt", &admin1),
            ("countryInfo.txt", &country),
        ] {
            if !dest.exists() {
                download_to_file(&format!("{GEONAMES_BASE}/{name}"), dest)?;
            }
        }
        Ok(())
    }

    pub fn default_data_dir() -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".cache").join("eidetic").join("geonames")
    }

    pub fn lookup(&self, lat: f64, lon: f64) -> Option<Place> {
        let mut best: Option<(usize, f64)> = None;
        for (i, c) in self.cities.iter().enumerate() {
            let d = haversine_m(lat, lon, c.lat, c.lon);
            match best {
                Some((_, bd)) if d >= bd => {}
                _ => best = Some((i, d)),
            }
        }
        let (i, d) = best?;
        let c = &self.cities[i];
        let country_name = self
            .country
            .get(&c.country_code)
            .cloned()
            .unwrap_or_else(|| c.country_code.clone());
        let admin1 = self
            .admin1
            .get(&(c.country_code.clone(), c.admin1_code.clone()))
            .cloned()
            .unwrap_or_else(|| c.admin1_code.clone());
        Some(Place {
            country_code: c.country_code.clone(),
            country_name,
            admin1,
            place: c.name.clone(),
            distance_m: d as f32,
        })
    }
}

fn download_to_file(url: &str, dest: &Path) -> Result<(), Error> {
    eprintln!("  GET {url}");
    let resp = ureq::get(url).call().map_err(|source| Error::Download {
        url: url.into(),
        source: Box::new(source),
    })?;
    let partial = dest.with_extension(format!(
        "{}.partial",
        dest.extension().and_then(|s| s.to_str()).unwrap_or("")
    ));
    let mut out = File::create(&partial).map_err(|source| Error::Io {
        path: partial.clone(),
        source,
    })?;
    let bytes = io::copy(&mut resp.into_reader(), &mut out).map_err(|source| Error::Io {
        path: partial.clone(),
        source,
    })?;
    fs::rename(&partial, dest).map_err(|source| Error::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    eprintln!("    {} bytes", bytes);
    Ok(())
}

fn download_and_extract_cities500(data_dir: &Path, dest: &Path) -> Result<(), Error> {
    let zip_path = data_dir.join("cities500.zip");
    download_to_file(&format!("{GEONAMES_BASE}/cities500.zip"), &zip_path)?;

    let zf = File::open(&zip_path).map_err(|source| Error::Io {
        path: zip_path.clone(),
        source,
    })?;
    let mut archive = zip::ZipArchive::new(zf).map_err(|source| Error::Zip {
        path: zip_path.clone(),
        source,
    })?;
    let mut entry = archive
        .by_name("cities500.txt")
        .map_err(|source| Error::Zip {
            path: zip_path.clone(),
            source,
        })?;

    let partial = dest.with_extension("txt.partial");
    let mut out = File::create(&partial).map_err(|source| Error::Io {
        path: partial.clone(),
        source,
    })?;
    io::copy(&mut entry, &mut out).map_err(|source| Error::Io {
        path: partial.clone(),
        source,
    })?;
    drop(entry);
    drop(archive);
    fs::rename(&partial, dest).map_err(|source| Error::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    fs::remove_file(&zip_path).map_err(|source| Error::Io {
        path: zip_path,
        source,
    })?;
    Ok(())
}

fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6_371_000.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().asin()
}

// countryInfo.txt: tab-separated, '#' comments. Col 0 = ISO alpha-2, col 4 = country name.
fn load_country_info(path: &Path) -> Result<HashMap<String, String>, Error> {
    let f = File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut out = HashMap::new();
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 5 {
            return Err(Error::Parse {
                path: path.to_path_buf(),
                line: i + 1,
                reason: format!("expected >=5 columns, got {}", cols.len()),
            });
        }
        out.insert(cols[0].to_string(), cols[4].to_string());
    }
    Ok(out)
}

// admin1CodesASCII.txt: tab-separated. Col 0 = "CC.CODE", col 1 = name.
fn load_admin1_codes(path: &Path) -> Result<HashMap<(String, String), String>, Error> {
    let f = File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut out = HashMap::new();
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 2 {
            return Err(Error::Parse {
                path: path.to_path_buf(),
                line: i + 1,
                reason: format!("expected >=2 columns, got {}", cols.len()),
            });
        }
        let Some((cc, code)) = cols[0].split_once('.') else {
            continue;
        };
        out.insert((cc.to_string(), code.to_string()), cols[1].to_string());
    }
    Ok(out)
}

// cities500.txt columns (0-indexed): 1 name, 4 lat, 5 lon, 8 country, 10 admin1.
fn load_cities500(path: &Path) -> Result<Vec<City>, Error> {
    let f = File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut cities = Vec::with_capacity(250_000);
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 11 {
            return Err(Error::Parse {
                path: path.to_path_buf(),
                line: i + 1,
                reason: format!("expected >=11 columns, got {}", cols.len()),
            });
        }
        let lat: f64 = cols[4].parse().map_err(|_| Error::Parse {
            path: path.to_path_buf(),
            line: i + 1,
            reason: format!("invalid latitude: {}", cols[4]),
        })?;
        let lon: f64 = cols[5].parse().map_err(|_| Error::Parse {
            path: path.to_path_buf(),
            line: i + 1,
            reason: format!("invalid longitude: {}", cols[5]),
        })?;
        cities.push(City {
            name: cols[1].to_string(),
            lat,
            lon,
            country_code: cols[8].to_string(),
            admin1_code: cols[10].to_string(),
        });
    }
    Ok(cities)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/geonames")
    }

    #[test]
    fn resolves_country_and_admin1_for_an_indian_town() {
        let g = Geocoder::open(&fixture_dir()).expect("open");
        let p = g.lookup(32.539, 75.972).expect("lookup");
        assert_eq!(p.place, "Dalhousie");
        assert_eq!(p.admin1, "Himachal Pradesh");
        assert_eq!(p.country_code, "IN");
        assert_eq!(p.country_name, "India");
        assert!(p.distance_m < 5_000.0, "distance was {}", p.distance_m);
    }

    #[test]
    fn picks_the_closer_of_two_candidate_cities() {
        let g = Geocoder::open(&fixture_dir()).expect("open");
        let p = g.lookup(32.21, 76.32).expect("lookup");
        assert_eq!(p.place, "Dharamsala");
    }

    #[test]
    fn missing_dataset_reports_clean_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = Geocoder::open(tmp.path()).err().expect("should fail");
        assert!(matches!(err, Error::DatasetMissing { .. }), "got {err:?}");
    }

    #[test]
    fn unknown_admin1_code_passes_through_as_raw_string() {
        let g = Geocoder {
            cities: vec![City {
                name: "Nowhere".into(),
                lat: 0.0,
                lon: 0.0,
                country_code: "ZZ".into(),
                admin1_code: "99".into(),
            }],
            admin1: HashMap::new(),
            country: HashMap::new(),
        };
        let p = g.lookup(0.0, 0.0).expect("lookup");
        assert_eq!(p.country_name, "ZZ");
        assert_eq!(p.admin1, "99");
    }

    #[test]
    fn ensure_dataset_is_a_noop_when_all_files_already_present() {
        Geocoder::ensure_dataset(&fixture_dir()).expect("ensure_dataset");
    }

    #[test]
    fn haversine_matches_known_great_circle_distances() {
        // Loose tolerances because we use mean-radius, not WGS-84.
        let sf_nyc = haversine_m(37.7749, -122.4194, 40.7128, -74.0060);
        assert!(
            (sf_nyc - 4_135_000.0).abs() < 15_000.0,
            "SF-NYC was {sf_nyc}"
        );
        let sf_la = haversine_m(37.7749, -122.4194, 34.0522, -118.2437);
        assert!((sf_la - 559_000.0).abs() < 5_000.0, "SF-LA was {sf_la}");
    }
}
