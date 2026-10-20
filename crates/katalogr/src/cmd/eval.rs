//! `katalogr eval run|sweep`: retrieval evaluation over golden sets.

use std::path::PathBuf;

use katalogr_dist::eval::{
    EvalParams, EvalRequest, SweepParam, apply, compare_report, default_tolerance, parse_sweep,
    read_baseline, run_fixtures, sweep, write_baseline,
};
use katalogr_dist::{Consistency, DistError, Options, Report};

use crate::cli::{Cli, EvalAction};
use crate::cmd::{options, resolve};
use crate::exit::ExitCode;
use crate::render;

const DEFAULT_SETS: &str = "fixtures/eval";

fn fixture_report() -> Report {
    Report {
        watermark: 0,
        pending: 0,
        stale: false,
        timed_out: false,
        queued: 0,
        warnings: Vec::new(),
    }
}

fn request(cli: &Cli, sets: &[String], k: &[u32], params: EvalParams) -> EvalRequest {
    let mut req = EvalRequest {
        sets: if sets.is_empty() {
            vec![PathBuf::from(DEFAULT_SETS)]
        } else {
            sets.iter().map(PathBuf::from).collect()
        },
        ks: k.to_vec(),
        params,
        ..EvalRequest::default()
    };
    if let Some(t) = cli.timeout {
        req.index_timeout = t;
    }
    req
}

/// What a project-mode read saw: the queue state after the queries.
fn project_report(k: &katalogr_dist::Katalogr) -> Result<Report, DistError> {
    k.status(&Options {
        consistency: Some(Consistency::StaleOk),
        timeout: None,
        diagnostic: false,
    })
}

pub fn run(cli: &Cli, action: &EvalAction) -> Result<ExitCode, DistError> {
    match action {
        EvalAction::Run {
            sets,
            project_sets,
            k,
            param,
            baseline,
            gate,
            write_baseline: write_to,
        } => {
            let mut params = EvalParams::default();
            for spec in param {
                let p = parse_sweep(spec)?;
                if p.values.len() != 1 {
                    return Err(DistError::Usage(format!(
                        "--param {spec}: give one value (use `eval sweep` for ranges)"
                    )));
                }
                apply(&mut params, &p.key, p.values[0])?;
            }
            let (mut report, meta, project_baseline) = if *project_sets {
                let kx = resolve(cli)?;
                let r = kx.eval_project(k, &params, &options(cli))?;
                let meta = project_report(&kx)?;
                (r, meta, Some(kx.eval_baseline_path()))
            } else {
                (
                    run_fixtures(&request(cli, sets, k, params))?,
                    fixture_report(),
                    None,
                )
            };
            if let Some(path) = write_to {
                write_baseline(std::path::Path::new(path), &report)?;
                render::eval_report(cli, report, &meta);
                return Ok(ExitCode::Ok);
            }
            let base_path = baseline
                .as_ref()
                .map(PathBuf::from)
                .or_else(|| project_baseline.filter(|p| p.is_file()));
            if *gate && base_path.is_none() {
                return Err(DistError::Usage(
                    "--gate needs a baseline (--baseline FILE)".into(),
                ));
            }
            let mut failed = false;
            if let Some(path) = base_path {
                let base = read_baseline(&path)?;
                let c = compare_report(&base, &report, &default_tolerance)?;
                failed = c.failed;
                report.comparison = Some(c);
            }
            render::eval_report(cli, report, &meta);
            Ok(if *gate && failed {
                ExitCode::Error
            } else {
                ExitCode::Ok
            })
        }
        EvalAction::Sweep {
            sets,
            project_sets,
            k,
            param,
            objective,
        } => {
            let ranges: Vec<SweepParam> = param
                .iter()
                .map(|s| parse_sweep(s))
                .collect::<Result<_, _>>()?;
            let (report, meta) = if *project_sets {
                let kx = resolve(cli)?;
                let r = kx.eval_project_sweep(
                    k,
                    &EvalParams::default(),
                    &ranges,
                    objective,
                    &options(cli),
                )?;
                let meta = project_report(&kx)?;
                (r, meta)
            } else {
                (
                    sweep(
                        &request(cli, sets, k, EvalParams::default()),
                        &ranges,
                        objective,
                    )?,
                    fixture_report(),
                )
            };
            render::sweep_report(cli, report, &meta);
            Ok(ExitCode::Ok)
        }
    }
}
