//! The meaning index: one int8 vector per passage (768 bytes plus a scale), written by the indexer
//! as a single file and searched by the app with a brute-force dot product, which takes a few
//! milliseconds for tens of thousands of passages.
//!
//! Scores are cosines after subtracting the mean passage vector. Raw cosines from this model
//! are all high and close (0.5 to 0.7), and "hub" passages such as word lists, minified code
//! and subtitles come out near the top for every query, even nonsense; centring spreads the
//! scores and drops the hubs below real matches.

use std::{
    fs,
    io::{self, Read, Write},
    path::Path,
};

use crate::embed::DIMS;

const MAGIC: &[u8; 8] = b"SEEKRVE1";

/// A vector as 8-bit integers and the scale that turns them back into floats.
pub fn quantize(v: &[f32]) -> (f32, Vec<i8>) {
    let max = v.iter().fold(0f32, |m, x| m.max(x.abs())).max(1e-9);
    let scale = max / 127.0;
    (scale, v.iter().map(|x| (x / scale).round() as i8).collect())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub path: String,
    pub line: u32,
    /// Cosine similarity around the mean passage: under 0.3 for unrelated text, 0.4 and up for
    /// a real match.
    pub score: f32,
}

#[derive(Default)]
pub struct Vectors {
    paths: Vec<String>,
    /// Per passage: index into `paths`, start line, scale.
    rows: Vec<(u32, u32, f32)>,
    data: Vec<i8>,
    /// Set by `load`; until then, as while the indexer builds the file, scores are raw cosines.
    centre: Option<Centre>,
}

struct Centre {
    mean: Vec<f32>,
    mean_sq: f32,
    /// Per passage: its dot product with the mean, and its length once the mean is taken away.
    rows: Vec<(f32, f32)>,
}

impl Centre {
    fn of(vectors: &Vectors) -> Option<Self> {
        if vectors.rows.is_empty() {
            return None;
        }
        let passages = || {
            vectors
                .data
                .as_chunks::<DIMS>()
                .0
                .iter()
                .zip(&vectors.rows)
                .map(|(v, &(_, _, scale))| v.map(|x| f32::from(x) * scale))
        };
        let mut mean = vec![0f32; DIMS];
        for v in passages() {
            for (m, x) in mean.iter_mut().zip(v) {
                *m += x;
            }
        }
        let n = vectors.rows.len() as f32;
        mean.iter_mut().for_each(|m| *m /= n);
        let mean_sq = dot(&mean, &mean);
        let rows = passages()
            .map(|v| {
                let with_mean = dot(&v, &mean);
                let left = (dot(&v, &v) - 2.0 * with_mean + mean_sq).max(1e-12).sqrt();
                (with_mean, left)
            })
            .collect();
        Some(Self {
            mean,
            mean_sq,
            rows,
        })
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

impl Vectors {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn push(&mut self, path: &str, line: u32, scale: f32, vector: &[i8]) {
        let index = match self.paths.last() {
            Some(last) if last == path => self.paths.len() - 1,
            _ => {
                self.paths.push(path.to_string());
                self.paths.len() - 1
            }
        };
        self.rows.push((index as u32, line, scale));
        self.data.extend_from_slice(vector);
        self.centre = None;
    }

    /// The best passage of each of the `limit` closest files to the unit vector `query`.
    pub fn search(&self, query: &[f32], limit: usize) -> Vec<Hit> {
        // (q - m)·(d - m) = q·d - q·m - d·m + m·m, over the two lengths without the mean.
        let centre = self.centre.as_ref().map(|c| {
            let with_mean = dot(query, &c.mean);
            let left = (dot(query, query) - 2.0 * with_mean + c.mean_sq)
                .max(1e-12)
                .sqrt();
            (c, with_mean, left)
        });
        let mut scored: Vec<(f32, usize)> = self
            .data
            .as_chunks::<DIMS>()
            .0
            .iter()
            .zip(&self.rows)
            .enumerate()
            .map(|(i, (v, &(_, _, scale)))| {
                let raw = scale
                    * v.iter()
                        .zip(query)
                        .map(|(&a, &b)| f32::from(a) * b)
                        .sum::<f32>();
                let score = match centre {
                    Some((c, q_mean, q_left)) => {
                        let (d_mean, d_left) = c.rows[i];
                        (raw - q_mean - d_mean + c.mean_sq) / (q_left * d_left)
                    }
                    None => raw,
                };
                (score, i)
            })
            .collect();
        scored.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
        let mut hits: Vec<Hit> = Vec::new();
        for (score, i) in scored {
            let (path, line, _) = self.rows[i];
            let path = &self.paths[path as usize];
            if hits.iter().any(|h| &h.path == path) {
                continue;
            }
            hits.push(Hit {
                path: path.clone(),
                line,
                score,
            });
            if hits.len() == limit {
                break;
            }
        }
        hits
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension("tmp");
        let mut out = io::BufWriter::new(fs::File::create(&tmp)?);
        out.write_all(MAGIC)?;
        out.write_all(&(self.paths.len() as u32).to_le_bytes())?;
        for p in &self.paths {
            out.write_all(&(p.len() as u32).to_le_bytes())?;
            out.write_all(p.as_bytes())?;
        }
        out.write_all(&(self.rows.len() as u32).to_le_bytes())?;
        for &(p, line, scale) in &self.rows {
            out.write_all(&p.to_le_bytes())?;
            out.write_all(&line.to_le_bytes())?;
            out.write_all(&scale.to_le_bytes())?;
        }
        out.write_all(&self.data.iter().map(|&b| b as u8).collect::<Vec<u8>>())?;
        out.into_inner()?.sync_all()?;
        fs::rename(tmp, path)
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        let mut r = &bytes[..];
        let bad = || io::Error::new(io::ErrorKind::InvalidData, "corrupt vector file");
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(bad());
        }
        let u32_ = |r: &mut &[u8]| -> io::Result<u32> {
            let mut b = [0u8; 4];
            r.read_exact(&mut b)?;
            Ok(u32::from_le_bytes(b))
        };
        let path_count = u32_(&mut r)? as usize;
        let mut paths = Vec::with_capacity(path_count.min(1 << 20));
        for _ in 0..path_count {
            let len = u32_(&mut r)? as usize;
            let text = r.get(..len).ok_or_else(bad)?;
            paths.push(String::from_utf8(text.to_vec()).map_err(|_| bad())?);
            r = &r[len..];
        }
        let row_count = u32_(&mut r)? as usize;
        let mut rows = Vec::with_capacity(row_count.min(1 << 24));
        for _ in 0..row_count {
            let p = u32_(&mut r)?;
            let line = u32_(&mut r)?;
            let scale = f32::from_bits(u32_(&mut r)?);
            if p as usize >= paths.len() {
                return Err(bad());
            }
            rows.push((p, line, scale));
        }
        if r.len() != row_count * DIMS {
            return Err(bad());
        }
        let data = r.iter().map(|&b| b as i8).collect();
        let mut vectors = Self {
            paths,
            rows,
            data,
            centre: None,
        };
        vectors.centre = Centre::of(&vectors);
        Ok(vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(seed: usize) -> Vec<f32> {
        let v: Vec<f32> = (0..DIMS)
            .map(|i| ((i * 7 + seed * 13) % 17) as f32 - 8.0)
            .collect();
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    }

    #[test]
    fn quantized_search_finds_the_nearest_file_and_round_trips() {
        let mut vectors = Vectors::default();
        for (path, seed) in [("a", 1), ("a", 2), ("b", 3), ("c", 4)] {
            let (scale, q) = quantize(&unit(seed));
            vectors.push(path, seed as u32, scale, &q);
        }
        let hits = vectors.search(&unit(3), 2);
        assert_eq!(hits[0].path, "b");
        assert!(
            (hits[0].score - 1.0).abs() < 0.02,
            "int8 keeps cosine close"
        );
        assert_eq!(hits.len(), 2);
        let file = std::env::temp_dir().join(format!("seekr-vectors-{}.bin", std::process::id()));
        vectors.save(&file).unwrap();
        let back = Vectors::load(&file).unwrap();
        assert_eq!(back.paths, vectors.paths);
        assert_eq!(back.rows, vectors.rows);
        assert_eq!(back.search(&unit(3), 1)[0].path, "b");
        fs::write(&file, b"SEEKRVE1garbage").unwrap();
        assert!(Vectors::load(&file).is_err());
        fs::remove_file(&file).unwrap();
    }

    #[test]
    fn centring_drops_a_passage_close_to_everything() {
        // Three topics sharing one strong direction, as this model's vectors do, and a hub
        // passage that is only that direction.
        let basis = |k: usize| -> Vec<f32> { (0..DIMS).map(|i| f32::from(i == k)).collect() };
        let topic = |k: usize| -> Vec<f32> {
            let v: Vec<f32> = basis(0)
                .iter()
                .zip(basis(k))
                .map(|(c, t)| c + 0.6 * t)
                .collect();
            let n = dot(&v, &v).sqrt();
            v.iter().map(|x| x / n).collect()
        };
        let mut vectors = Vectors::default();
        for (path, v) in [
            ("a", topic(1)),
            ("b", topic(2)),
            ("c", topic(3)),
            ("hub", basis(0)),
        ] {
            let (scale, q) = quantize(&v);
            vectors.push(path, 1, scale, &q);
        }
        let score = |hits: &[Hit], path: &str| hits.iter().find(|h| h.path == path).unwrap().score;
        let raw = vectors.search(&topic(2), 4);
        assert_eq!(raw[0].path, "b");
        assert!(
            score(&raw, "hub") > 0.8,
            "raw cosine puts the hub near the match"
        );
        vectors.centre = Centre::of(&vectors);
        let centred = vectors.search(&topic(2), 4);
        assert_eq!(centred[0].path, "b");
        assert!(centred[0].score > 0.9);
        assert!(score(&centred, "hub") < 0.1, "{centred:?}");
    }
}
