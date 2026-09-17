// SPDX-License-Identifier: AGPL-3.0-or-later
//! Solver selection: which `quip-<backend>-<algorithm>` binary the coordinator
//! spawns for each mining backend.
//!
//! The coordinator does not validate the name. It resolves each section's
//! `binary` through `posix_spawnp`, so any name is accepted and a name the
//! image does not carry fails late — the supervisor logs `miner spawn failed
//! … No such file or directory` only after the chain connect, preflight and
//! funding all succeed. That makes the *available* list the thing worth getting
//! right: it comes from the image, not from a list compiled into this app.
//!
//! Verified against quip-miner v0.3.3: `[cpu].binary`, `[cuda.N].binary` and
//! `[metal].binary` are each honored. `[gpu]` is not a backend section in v0.3
//! — the coordinator names them as `[cpu]`, `[cuda.N]`, `[metal]`,
//! `[dwave]`/`[qpu]`.
//!
//! `CATALOG` is keyed on the algorithm suffix, not the whole binary name, so
//! one table serves every backend: `quip-cuda-sa` and `quip-cpu-sa` are the
//! same algorithm on different hardware. A suffix the table does not know is
//! still offered, unlabeled, so shipping a new solver never waits on an app
//! release.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Cpu,
    Cuda,
    Metal,
}

impl Backend {
    /// Binary-name prefix. The text after it is the algorithm suffix.
    pub fn prefix(self) -> &'static str {
        match self {
            Backend::Cpu => "quip-cpu-",
            Backend::Cuda => "quip-cuda-",
            Backend::Metal => "quip-metal-",
        }
    }

    /// Binary used when the operator has not chosen one and the preferred
    /// solver is not installed. Matches the `binary` each backend's own config
    /// defaults to.
    pub fn fallback_solver(self) -> &'static str {
        match self {
            Backend::Cpu => "quip-cpu-sa",
            Backend::Cuda => "quip-cuda-sa",
            Backend::Metal => "quip-metal-sa",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Track {
    Production,
    Experimental,
    /// Present in the image but absent from `CATALOG` — newer than this build.
    Unknown,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Solver {
    pub binary: String,
    pub algorithm: String,
    pub track: Track,
}

/// Algorithm text and track per name suffix. Labels only: presence here does
/// not mean any image carries it, and absence does not hide it.
///
/// Track is a property of the algorithm, so a CUDA or Metal build of an
/// experimental algorithm is labeled experimental too.
const CATALOG: &[(&str, &str, Track)] = &[
    ("sa", "simulated annealing (Metropolis)", Track::Production),
    ("gibbs", "single-site heat-bath Gibbs", Track::Production),
    ("sb", "discrete Simulated Bifurcation", Track::Production),
    (
        "bsb",
        "ballistic Simulated Bifurcation",
        Track::Experimental,
    ),
    (
        "hdsb",
        "heated discrete Simulated Bifurcation",
        Track::Experimental,
    ),
    (
        "hbsb",
        "heated ballistic Simulated Bifurcation",
        Track::Experimental,
    ),
    (
        "gbsb",
        "generalized ballistic Simulated Bifurcation, edge-of-chaos control",
        Track::Experimental,
    ),
    (
        "gdsb",
        "edge-of-chaos control on the discrete coupling",
        Track::Experimental,
    ),
    (
        "tedsb",
        "tabu-enhanced discrete Simulated Bifurcation",
        Track::Experimental,
    ),
    (
        "sbqa",
        "replica-ring discrete Simulated Bifurcation",
        Track::Experimental,
    ),
    (
        "ggdsb",
        "globally guided discrete Simulated Bifurcation",
        Track::Experimental,
    ),
    (
        "fsa",
        "simulated annealing with tabulated Metropolis thresholds",
        Track::Experimental,
    ),
    (
        "msa",
        "multi-spin coded simulated annealing, 64 reads per word",
        Track::Experimental,
    ),
    (
        "mps",
        "tensor network: imaginary-time TEBD with exact sampling",
        Track::Experimental,
    ),
    (
        "mfa",
        "mean-field annealing (the same kernel at bond dimension 1)",
        Track::Experimental,
    ),
    (
        "flatiron",
        "belief-propagation tensor network on the problem graph",
        Track::Experimental,
    ),
];

/// Algorithm an unset picker runs when this backend's build of it is installed.
const PREFERRED_SUFFIX: &str = "msa";

/// What "Default" means for a backend, given what is installed: the preferred
/// solver when present, otherwise the backend's fallback.
pub fn default_among(backend: Backend, available: &[Solver]) -> String {
    let preferred = format!("{}{PREFERRED_SUFFIX}", backend.prefix());
    if available.iter().any(|s| s.binary == preferred) {
        preferred
    } else {
        backend.fallback_solver().to_string()
    }
}

fn describe(backend: Backend, binary: &str) -> Solver {
    let suffix = binary.strip_prefix(backend.prefix()).unwrap_or("");
    let (algorithm, track) = CATALOG
        .iter()
        .find(|(name, _, _)| *name == suffix)
        .map(|(_, algorithm, track)| ((*algorithm).to_string(), *track))
        .unwrap_or_else(|| (String::new(), Track::Unknown));
    Solver {
        binary: binary.to_string(),
        algorithm,
        track,
    }
}

/// Pick this backend's solvers out of a directory listing and describe them.
///
/// Production first, then experimental, then unknown, alphabetical within each.
/// A flat list mixes the tracks, so ordering by track is what keeps the
/// supported choices together at the top instead of interleaved with the
/// experimental ones wherever `ls` happened to put them.
pub fn solvers_from_listing(backend: Backend, listing: &str) -> Vec<Solver> {
    let prefix = backend.prefix();
    let mut found: Vec<Solver> = listing
        .lines()
        .map(str::trim)
        .filter(|name| name.starts_with(prefix) && name.len() > prefix.len())
        .map(|name| describe(backend, name))
        .collect();
    found.sort_by(|a, b| a.binary.cmp(&b.binary));
    found.dedup_by(|a, b| a.binary == b.binary);
    found.sort_by_key(|s| match s.track {
        Track::Production => 0,
        Track::Experimental => 1,
        Track::Unknown => 2,
    });
    found
}

/// What the pickers show, plus why the list may be short.
#[derive(Serialize, Clone, Debug)]
pub struct SolverCatalog {
    pub backend: Backend,
    pub solvers: Vec<Solver>,
    pub selected: String,
    pub default_solver: String,
    /// Set when the source could not be read, so the UI can explain a list that
    /// holds only the current selection instead of silently offering one entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

impl SolverCatalog {
    /// Fall back to just the current selection. Offering names the image may
    /// not carry would hand the operator a choice that fails minutes later,
    /// after the chain connect and funding, with no hint that the pick caused it.
    fn only_selected(backend: Backend, selected: &str, reason: impl Into<String>) -> Self {
        SolverCatalog {
            backend,
            solvers: vec![describe(backend, selected)],
            selected: selected.to_string(),
            default_solver: backend.fallback_solver().to_string(),
            unavailable: Some(reason.into()),
        }
    }
}

/// Where the miner binaries live inside the image, and the directory the
/// coordinator's PATH lookup finds them in.
const IMAGE_BIN_DIR: &str = "/usr/local/bin";

/// List the solvers the operator can actually run for one backend.
///
/// Docker reads the image, preferring the running container because that costs
/// nothing and is unambiguously the image in use. Falling back to `docker run`
/// is deliberately limited to an image already pulled: `pull_policy: always`
/// means Start fetches it, and enumerating must never trigger a multi-gigabyte
/// pull from opening a settings dropdown.
///
/// Metal has no container — it is a macOS Native backend — so it always reads
/// the bundle directory regardless of run mode.
#[tauri::command]
pub async fn list_solvers(backend: Backend) -> SolverCatalog {
    let settings = crate::settings::load_settings();
    let saved = selected_solver(&settings.node_config, backend);

    let listing = if reads_bundle(backend, &settings.run_mode) {
        native_listing()
    } else {
        docker_listing(settings.image_tag).await
    };

    match listing {
        Ok(listing) => {
            let solvers = solvers_from_listing(backend, &listing);
            if solvers.is_empty() {
                return SolverCatalog::only_selected(
                    backend,
                    &saved.unwrap_or_else(|| backend.fallback_solver().to_string()),
                    format!("no {}* binaries found", backend.prefix()),
                );
            }
            let default_solver = default_among(backend, &solvers);
            SolverCatalog {
                backend,
                selected: saved.unwrap_or_else(|| default_solver.clone()),
                solvers,
                default_solver,
                unavailable: None,
            }
        }
        Err(reason) => SolverCatalog::only_selected(
            backend,
            &saved.unwrap_or_else(|| backend.fallback_solver().to_string()),
            reason,
        ),
    }
}

/// Metal has no container, and Native runs every backend from the bundle.
fn reads_bundle(backend: Backend, run_mode: &crate::settings::RunMode) -> bool {
    backend == Backend::Metal || *run_mode == crate::settings::RunMode::Native
}

/// Fill each unset solver with the default this install resolves to, so the
/// rendered config names the preferred solver when it is installed.
///
/// Runs at Start, on a copy of the settings: the saved choice stays unset, so
/// a later image or bundle that drops the preferred solver falls back instead
/// of pinning a name that no longer exists. A source that cannot be read
/// leaves the field unset, which renders the fallback exactly as before.
pub async fn resolve_unset_solvers(
    config: &mut crate::settings::NodeConfig,
    run_mode: &crate::settings::RunMode,
    image_tag: crate::settings::ImageTag,
) {
    let bundle = native_listing().ok();
    let image = if *run_mode == crate::settings::RunMode::Docker {
        docker_listing(image_tag).await.ok()
    } else {
        None
    };
    for backend in [Backend::Cpu, Backend::Cuda, Backend::Metal] {
        let slot = match backend {
            Backend::Cpu => &mut config.cpu_solver,
            Backend::Cuda => &mut config.cuda_solver,
            Backend::Metal => &mut config.metal_solver,
        };
        let listing = if reads_bundle(backend, run_mode) {
            bundle.as_deref()
        } else {
            image.as_deref()
        };
        fill_unset(slot, backend, listing);
    }
}

/// Set an unset slot to the preferred solver when `listing` carries it. The
/// fallback is left unset rather than written, so it renders as it always has.
fn fill_unset(slot: &mut Option<String>, backend: Backend, listing: Option<&str>) {
    let (None, Some(listing)) = (&slot, listing) else {
        return;
    };
    let default = default_among(backend, &solvers_from_listing(backend, listing));
    if default != backend.fallback_solver() {
        *slot = Some(default);
    }
}

/// The saved choice for a backend, or `None` for "whatever the image defaults to".
pub fn selected_solver(config: &crate::settings::NodeConfig, backend: Backend) -> Option<String> {
    match backend {
        Backend::Cpu => config.cpu_solver.clone(),
        Backend::Cuda => config.cuda_solver.clone(),
        Backend::Metal => config.metal_solver.clone(),
    }
}

/// Read the bundled miner directory. Native ships every backend's binaries in
/// one directory.
fn native_listing() -> Result<String, String> {
    let dir = crate::native::bin_dir();
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    Ok(entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("\n"))
}

async fn docker_listing(image_tag: crate::settings::ImageTag) -> Result<String, String> {
    let container = format!("quip-{}", image_tag.service());
    if let Ok(listing) = run_docker(&["exec", &container, "ls", "-1", IMAGE_BIN_DIR]).await {
        return Ok(listing);
    }
    let image = crate::compose::image_for_tag(image_tag);
    // `docker image inspect` is the guard that keeps this from pulling: it
    // fails fast when the image is absent instead of `docker run` fetching it.
    let reference = local_image_reference(image).await?;
    run_docker(&[
        "run",
        "--rm",
        "--entrypoint",
        "ls",
        &reference,
        "-1",
        IMAGE_BIN_DIR,
    ])
    .await
}

/// Find a locally present tag for `image`. The stack resolves channel tags at
/// Start; this only has to name something already on disk, so it checks the
/// tags the compose file can select rather than querying the registry.
async fn local_image_reference(image: &str) -> Result<String, String> {
    for tag in ["beta", "latest"] {
        let reference = format!("{image}:{tag}");
        if run_docker(&["image", "inspect", &reference]).await.is_ok() {
            return Ok(reference);
        }
    }
    Err("miner image not pulled yet — start the node once".to_string())
}

async fn run_docker(args: &[&str]) -> Result<String, String> {
    let mut command = crate::cmd::new("docker");
    command.args(args);
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(20), command.output())
        .await
        .map_err(|_| "docker timed out".to_string())?
        .map_err(|e| format!("could not run Docker: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly the `/usr/local/bin` listing from quip-miner-cuda:v0.3.3.
    const V033_CUDA_IMAGE: &str = "quip-coordinator
quip-cpu-bsb
quip-cpu-flatiron
quip-cpu-fsa
quip-cpu-gbsb
quip-cpu-gdsb
quip-cpu-ggdsb
quip-cpu-gibbs
quip-cpu-hbsb
quip-cpu-hdsb
quip-cpu-mfa
quip-cpu-mps
quip-cpu-msa
quip-cpu-sa
quip-cpu-sb
quip-cpu-sbqa
quip-cpu-tedsb
quip-cuda-gibbs
quip-cuda-sa";

    #[test]
    fn catalog_has_no_duplicate_suffixes() {
        let mut names: Vec<&str> = CATALOG.iter().map(|(n, _, _)| *n).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate suffix in CATALOG");
        assert_eq!(total, 16);
        assert_eq!(
            CATALOG
                .iter()
                .filter(|(_, _, t)| *t == Track::Production)
                .count(),
            3
        );
    }

    /// The catalog must cover every CPU solver v0.3.3 actually ships, or the
    /// picker would show real binaries as unlabeled unknowns.
    #[test]
    fn every_v033_cpu_solver_is_labeled() {
        let got = solvers_from_listing(Backend::Cpu, V033_CUDA_IMAGE);
        assert_eq!(got.len(), 16);
        let unlabeled: Vec<&str> = got
            .iter()
            .filter(|s| s.track == Track::Unknown)
            .map(|s| s.binary.as_str())
            .collect();
        assert!(unlabeled.is_empty(), "unlabeled: {unlabeled:?}");
    }

    /// One suffix table serves every backend: CUDA reuses CPU's algorithm
    /// names, so `quip-cuda-sa` must come out labeled without its own entry.
    #[test]
    fn cuda_solvers_reuse_the_shared_algorithm_labels() {
        let got = solvers_from_listing(Backend::Cuda, V033_CUDA_IMAGE);
        assert_eq!(
            got.iter().map(|s| s.binary.as_str()).collect::<Vec<_>>(),
            ["quip-cuda-gibbs", "quip-cuda-sa"]
        );
        assert_eq!(got[1].algorithm, "simulated annealing (Metropolis)");
        assert_eq!(got[1].track, Track::Production);
    }

    /// Backends must not pick up each other's binaries — the CUDA image holds
    /// both, and a `quip-cpu-*` name in the `[cuda.N]` section would spawn a
    /// CPU miner on the GPU device slot.
    #[test]
    fn backends_do_not_leak_into_each_other() {
        let cpu = solvers_from_listing(Backend::Cpu, V033_CUDA_IMAGE);
        assert!(cpu.iter().all(|s| s.binary.starts_with("quip-cpu-")));
        let cuda = solvers_from_listing(Backend::Cuda, V033_CUDA_IMAGE);
        assert!(cuda.iter().all(|s| s.binary.starts_with("quip-cuda-")));
        // Metal ships in the macOS bundle, never in the Linux image.
        assert!(solvers_from_listing(Backend::Metal, V033_CUDA_IMAGE).is_empty());
    }

    #[test]
    fn metal_solvers_are_labeled_from_the_same_table() {
        let got = solvers_from_listing(Backend::Metal, "quip-metal-sa\nquip-metal-mps\n");
        assert_eq!(got[0].binary, "quip-metal-sa");
        assert_eq!(got[0].track, Track::Production);
        assert_eq!(got[1].binary, "quip-metal-mps");
        assert_eq!(got[1].track, Track::Experimental);
    }

    /// v0.3.3 ships `quip-cpu-msa` but no CUDA build of it, so CPU defaults
    /// to msa while CUDA keeps its fallback.
    #[test]
    fn default_prefers_msa_only_where_installed() {
        let cpu = solvers_from_listing(Backend::Cpu, V033_CUDA_IMAGE);
        assert_eq!(default_among(Backend::Cpu, &cpu), "quip-cpu-msa");
        let cuda = solvers_from_listing(Backend::Cuda, V033_CUDA_IMAGE);
        assert_eq!(default_among(Backend::Cuda, &cuda), "quip-cuda-sa");
        // Nothing read yet: the fallback, never a name that may not exist.
        assert_eq!(default_among(Backend::Metal, &[]), "quip-metal-sa");
    }

    /// Start fills only unset slots, and only with a solver the source lists.
    #[test]
    fn fill_unset_respects_choices_and_unreadable_sources() {
        let mut unset = None;
        fill_unset(&mut unset, Backend::Cpu, Some(V033_CUDA_IMAGE));
        assert_eq!(unset.as_deref(), Some("quip-cpu-msa"));

        let mut chosen = Some("quip-cpu-sa".to_string());
        fill_unset(&mut chosen, Backend::Cpu, Some(V033_CUDA_IMAGE));
        assert_eq!(chosen.as_deref(), Some("quip-cpu-sa"));

        let mut unread = None;
        fill_unset(&mut unread, Backend::Cpu, None);
        assert!(unread.is_none());

        // No CUDA msa in v0.3.3: stay unset so `[cuda.N]` keeps no binary key.
        let mut cuda = None;
        fill_unset(&mut cuda, Backend::Cuda, Some(V033_CUDA_IMAGE));
        assert!(cuda.is_none());
    }

    /// The prefix alone is not a solver name, and must not become an entry the
    /// coordinator would then try to spawn.
    #[test]
    fn bare_prefix_is_not_a_solver() {
        assert!(solvers_from_listing(Backend::Cpu, "quip-cpu-\nquip-cpu\n").is_empty());
    }

    #[test]
    fn production_sorts_ahead_of_experimental_and_unknown() {
        let got = solvers_from_listing(
            Backend::Cpu,
            "quip-cpu-mps\nquip-cpu-neverheardofit\nquip-cpu-sa\n",
        );
        assert_eq!(
            got.iter().map(|s| s.track).collect::<Vec<_>>(),
            [Track::Production, Track::Experimental, Track::Unknown]
        );
        // A binary the app has never heard of is still offered, unlabeled.
        assert_eq!(got[2].binary, "quip-cpu-neverheardofit");
        assert!(got[2].algorithm.is_empty());
    }

    #[test]
    fn unreadable_source_still_offers_the_current_selection() {
        for backend in [Backend::Cpu, Backend::Cuda, Backend::Metal] {
            let c = SolverCatalog::only_selected(
                backend,
                backend.fallback_solver(),
                "image not pulled yet",
            );
            assert_eq!(c.solvers.len(), 1);
            assert_eq!(c.solvers[0].binary, backend.fallback_solver());
            // Every backend's default must be a real catalog entry, or a fresh
            // install would render its own setting as an unknown solver.
            assert_eq!(c.solvers[0].track, Track::Production);
            assert!(c.unavailable.is_some());
        }
    }
}
