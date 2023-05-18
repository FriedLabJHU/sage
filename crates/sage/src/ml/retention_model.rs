//! Retention time prediction using linear regression
//!
//! See Klammer et al., Anal. Chem. 2007, 79, 16, 6111–6118
//! https://doi.org/10.1021/ac070262k

use super::{gauss::Gauss, matrix::Matrix};
use crate::database::IndexedDatabase;
use crate::mass::VALID_AA;
use crate::peptide::Peptide;
use crate::scoring::Feature;
use rayon::prelude::*;

/// Try to fit a retention time prediction model
pub fn predict(db: &IndexedDatabase, features: &mut [Feature]) -> Option<()> {
    // Training LR might fail - not enough values, or r-squared is < 0.7
    let lr = RetentionModel::fit(db, features)?;
    features.par_iter_mut().for_each(|feat| {
        // LR can sometimes predict crazy values - clamp predicted RT
        let rt = lr.predict_peptide(db, feat);
        let bounded = rt.clamp(0.0, 1.0) as f32;
        feat.predicted_rt = bounded;
        feat.delta_rt_model = (feat.aligned_rt - bounded).abs();
    });
    Some(())
}
pub struct RetentionModel {
    beta: Vec<f64>,
    map: [usize; 26],
    pub r2: f64,
}

const FEATURES: usize = VALID_AA.len() * 3 + 3;
const N_TERMINAL: usize = VALID_AA.len();
const C_TERMINAL: usize = VALID_AA.len() * 2;
const PEPTIDE_LEN: usize = FEATURES - 3;
const PEPTIDE_MASS: usize = FEATURES - 2;
const INTERCEPT: usize = FEATURES - 1;

impl RetentionModel {
    /// One-hot encoding of peptide sequences into feature vector
    /// Note that this currently does not take into account any modifications
    fn embed(peptide: &Peptide, map: &[usize; 26]) -> [f64; FEATURES] {
        let mut embedding = [0.0; FEATURES];
        let cterm = peptide.sequence.len().saturating_sub(3);
        for (aa_idx, residue) in peptide.sequence.iter().enumerate() {
            let idx = map[(residue - b'A') as usize];
            embedding[idx] += 1.0;
            // Embed N- and C-terminal AA's (2 on each end, excluding K/R)
            match aa_idx {
                0 | 1 => embedding[N_TERMINAL + idx] += 1.0,
                x if x == cterm || x == cterm + 1 => embedding[C_TERMINAL + idx] += 1.0,
                _ => {}
            }
        }
        embedding[PEPTIDE_LEN] = peptide.sequence.len() as f64;
        embedding[PEPTIDE_MASS] = (peptide.monoisotopic as f64).ln_1p();
        embedding[INTERCEPT] = 1.0;
        embedding
    }

    /// Attempt to fit a linear regression model: peptide sequence ~ retention time
    pub fn fit(db: &IndexedDatabase, training_set: &[Feature]) -> Option<Self> {
        // Create a mapping from amino acid character to vector embedding
        let mut map = [0; 26];
        for (idx, aa) in VALID_AA.iter().enumerate() {
            map[(aa - b'A') as usize] = idx;
        }

        let rt = training_set
            .par_iter()
            .filter(|feat| feat.label == 1 && feat.spectrum_q <= 0.01)
            .map(|psm| psm.aligned_rt as f64)
            .collect::<Vec<f64>>();

        let rt_mean = rt.iter().sum::<f64>() / rt.len() as f64;
        let rt_var = rt.iter().map(|rt| (rt - rt_mean).powi(2)).sum::<f64>();

        let rt = Matrix::col_vector(rt);

        let features = training_set
            .par_iter()
            .filter(|feat| feat.label == 1 && feat.spectrum_q <= 0.01)
            .flat_map_iter(|psm| Self::embed(&db[psm.peptide_idx], &map))
            .collect::<Vec<_>>();

        let rows = features.len() / FEATURES;
        let features = Matrix::new(features, rows, FEATURES);

        let f_t = features.transpose();
        let mut cov = f_t.dot(&features);
        let b = f_t.dot(&rt);

        for i in 0..cov.cols {
            cov[(i, i)] += 0.1;
        }

        let beta = Gauss::solve(cov, b)?;

        let predicted_rt = features.dot(&beta).take();
        let sum_squared_error = predicted_rt
            .iter()
            .zip(rt.take())
            .map(|(pred, act)| (pred - act).powi(2))
            .sum::<f64>();

        let r2 = 1.0 - (sum_squared_error / rt_var);
        log::info!("- fit retention time model, rsq = {}", r2);
        Some(Self {
            beta: beta.take(),
            map,
            r2,
        })
    }

    /// Predict retention times for a collection of PSMs
    pub fn predict_peptide(&self, db: &IndexedDatabase, psm: &Feature) -> f64 {
        let v = Self::embed(&db[psm.peptide_idx], &self.map);
        v.into_iter()
            .zip(&self.beta)
            .fold(0.0f64, |sum, (x, y)| sum + x * y)
    }
}
