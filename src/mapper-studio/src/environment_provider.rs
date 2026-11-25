//! Source-bound monument and tunnel environment updates and placement proposals.
//! The owning generation stages supply lifecycle and preprocessing facts.
//! Prepared updates use the scene's revision checks for atomic publication.
use crate::{
    environment_checks::{Prepared, Registration},
    environment_runtime::{Lifecycle, Verified},
    monument_attributes::{Attributes, EnvironmentRecord},
    monument_source,
    prefab_assets::Catalog,
    prefab_textures::Bindings,
};
use papyrus_terrain::environment::{self, Shape, Transform, Verdict, scene};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, sync::Arc};
mod candidate;
pub use candidate::Candidate;
mod water;
pub use water::WaterCandidate;
pub mod offshore;
mod tunnels;
pub use tunnels::{TunnelError, TunnelLimits, TunnelReport};

pub const VERSION: &str = "papyrus-environment-provider-3";
/// Synthetic index key, never a captured Unity component. Captured object IDs
/// exclude i64::MIN. An unmodeled occurrence may gain volumes/change type masks.
pub const UNMODELED_COMPONENT: i64 = i64::MIN;
const MAX_ACTORS: usize = 65536;
const MAX_VOLUMES: usize = 131072;
const MAX_WORK: usize = 128_000_000;
// Reserve before calling the bounded hull builder. Cache hits do
// not change whether the same request fits its allowance.
const HULL_RESERVE: usize = 2_000_000;

#[derive(Clone, Debug)]
pub struct Occurrence {
    /// World-global occurrence ID, not a prefab-local collider or target ID.
    pub id: u64,
    pub prefab: String,
    /// Final post-spawn frame, after the owning adapter's transform policy.
    pub pose: Transform,
    /// Authoritative producer state, not a requested UI action or terrain-cache
    /// status. Confirm inactive/removed states before suppressing their volumes.
    pub lifecycle: Lifecycle,
}
pub struct Source {
    runtime: Verified,
    templates: monument_source::Source,
    sha256: String,
    water: Option<crate::placement_water::Model>,
    catalog_sha256: String,
    tunnels: Option<Arc<crate::tunnel_source::Source>>,
    river_environments: BTreeMap<String, Prepared>,
    monument_geometry: Option<monument_source::Source>,
}
pub struct Request {
    context: scene::Context,
    stage: i64,
    inputs_sha256: String,
    occurrences_sha256: String,
    actors: Vec<Occurrence>,
    publication_order: Vec<u64>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub engine: &'static str,
    pub sources_sha256: String,
    pub inputs_sha256: String,
    pub occurrences_sha256: String,
    pub stage: i64,
    pub actors: usize,
    pub volumes: usize,
    pub unresolved_geometry: usize,
    pub preparation_work_reserved: usize,
    pub index_work: usize,
}
pub struct Update {
    update: scene::PreparedUpdate,
    pub report: Report,
}
impl Update {
    pub fn into_parts(self) -> (scene::PreparedUpdate, Report) {
        (self.update, self.report)
    }
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn source_root_matches(bindings_root: &Path, verified_root: &Path) -> Result<bool, String> {
    let root = bindings_root
        .canonicalize()
        .map_err(|e| format!("Cannot resolve environment source folder: {e}"))?;
    Ok(root == verified_root)
}
impl Source {
    /// Catalog/bindings must come from their normal verified loaders. Bindings
    /// seal their installation root and catalog receipt. Own this on a worker
    /// inside the generation operation's installation::VerificationScope.
    pub fn load(
        runtime: Verified,
        catalog: &Catalog,
        bindings: &Bindings,
        snapshot: &Path,
        expected_sha256: &str,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if cancelled() {
            return Ok(None);
        }
        catalog.installation.validate()?;
        if !hash(expected_sha256)
            || runtime.native() != &catalog.installation.native
            || bindings.catalog_sha256() != catalog.sha256
            || !source_root_matches(bindings.installation_root(), runtime.root())?
        {
            return Err("Environment provider has mismatched installation/source bindings".into());
        }
        let templates = monument_source::Source::load(snapshot, catalog, bindings, &cancelled)?;
        if cancelled() {
            return Ok(None);
        }
        if templates.sha256 != expected_sha256 {
            return Err("Environment prefab snapshot differs from the requested receipt".into());
        }
        let water = crate::placement_water::Model::for_runtime(&runtime)?;
        let water_sha = water
            .as_ref()
            .map(crate::placement_water::Model::fingerprint)
            .transpose()?;
        let bytes = serde_json::to_vec(&(
            VERSION,
            environment::VERSION,
            runtime.fingerprint()?,
            &catalog.sha256,
            &bindings.sha256,
            &templates.sha256,
            water_sha,
        ))
        .map_err(|e| e.to_string())?;
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        Ok(Some(Self {
            runtime,
            templates,
            sha256,
            water,
            catalog_sha256: catalog.sha256.clone(),
            tunnels: None,
            river_environments: BTreeMap::new(),
            monument_geometry: None,
        }))
    }
    /// Bind the already-prepared tunnel pool to this world's environment source.
    /// Call before constructing the scene or any requests. Installation receipts
    /// are revalidated through the operation's verification scope.
    pub fn with_tunnels(
        mut self,
        tunnels: Arc<crate::tunnel_source::Source>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if self.tunnels.is_some() {
            return Err("Environment tunnel source is already bound".into());
        }
        if tunnels.catalog_sha256 != self.catalog_sha256
            || tunnels.monument_sha256 != self.templates.sha256
        {
            return Err(
                "Environment tunnel source has mismatched catalog or monument bindings".into(),
            );
        }
        if cancelled() {
            return Ok(None);
        }
        tunnels.verify(self.runtime.root(), &cancelled)?;
        let bytes = serde_json::to_vec(&(VERSION, &self.sha256, &tunnels.sha256))
            .map_err(|e| e.to_string())?;
        self.sha256 = format!("{:x}", Sha256::digest(bytes));
        self.tunnels = Some(tunnels);
        Ok((!cancelled()).then_some(self))
    }
    /// Supplement a pinned placement snapshot with verified collision buffers.
    /// Authored volume frames must still match the pinned snapshot exactly.
    pub(crate) fn with_monument_geometry(
        mut self,
        catalog: &Catalog,
        bindings: &Bindings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if self.monument_geometry.is_some()
            || catalog.sha256 != self.catalog_sha256
            || bindings.catalog_sha256() != self.catalog_sha256
            || !source_root_matches(bindings.installation_root(), self.runtime.root())?
        {
            return Err("Monument environment geometry bindings changed".into());
        }
        if crate::monument_extract::has_environment_meshes(&self.templates) {
            return Ok((!cancelled()).then_some(self));
        }
        let Some(geometry) = crate::monument_extract::prepare_environment(
            self.runtime.root(),
            catalog,
            bindings,
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        if geometry.prefabs.keys().ne(self.templates.prefabs.keys()) {
            return Err("Monument collision roster differs from the pinned snapshot".into());
        }
        for (path, prefab) in &self.templates.prefabs {
            let original = prefab
                .placement_attributes
                .as_ref()
                .and_then(|a| a.environment.as_ref());
            let captured = geometry.prefabs[path]
                .placement_attributes
                .as_ref()
                .and_then(|a| a.environment.as_ref());
            let Some(captured) = captured else {
                return Err(format!("Monument collision facts are missing for {path}"));
            };
            // Older placement snapshots omit environment data entirely. The
            // supplemental loader verifies the same installation and catalog.
            if original.is_some_and(|original| {
                original.root_transform != captured.root_transform
                    || original.volumes != captured.volumes
                    || original
                        .collider_meshes
                        .as_ref()
                        .is_some_and(|meshes| Some(meshes) != captured.collider_meshes.as_ref())
            }) {
                return Err(format!(
                    "Monument collision frames differ from the pinned snapshot for {path}"
                ));
            }
        }
        let bytes = serde_json::to_vec(&(VERSION, &self.sha256, &geometry.sha256))
            .map_err(|error| error.to_string())?;
        self.sha256 = format!("{:x}", Sha256::digest(bytes));
        self.monument_geometry = Some(geometry);
        Ok((!cancelled()).then_some(self))
    }
    /// Register source-bound river objects generated before the dungeon stage.
    pub(crate) fn with_river_objects(
        mut self,
        catalog: &Catalog,
        recipe: &crate::river_objects::Recipe,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
        if catalog.sha256 != self.catalog_sha256
            || recipe.catalog_sha256 != catalog.sha256
            || self.runtime.native() != &catalog.installation.native
            || !self.river_environments.is_empty()
        {
            return Err("River environment source bindings changed".into());
        }
        let source = recipe.load(self.runtime.root(), &catalog.installation, &cancelled)?;
        let environments = source
            .environments
            .as_ref()
            .ok_or("River object environment capture is unavailable")?;
        let mut work = 0usize;
        for (path, environment) in environments {
            let attributes = Attributes {
                version: 3,
                root_decor: vec![],
                terrain_checks: vec![],
                water_checks: vec![],
                path_connectors: vec![],
                placement_blockers: Some(vec![]),
                environment: Some(environment.clone()),
                issues: vec![],
            };
            work = work
                .checked_add(preparation_cost(&attributes)?)
                .filter(|work| *work <= MAX_WORK)
                .ok_or("River environment preparation exceeds its work allowance")?;
            let Some(prepared) = Prepared::for_registration(&attributes, &cancelled)
                .map_err(|error| format!("River environment {path}: {error}"))?
            else {
                return Ok(None);
            };
            self.river_environments.insert(path.clone(), prepared);
        }
        let bytes = serde_json::to_vec(&(VERSION, &self.sha256, &source.sha256))
            .map_err(|e| e.to_string())?;
        self.sha256 = format!("{:x}", Sha256::digest(bytes));
        Ok((!cancelled()).then_some(self))
    }
    /// Monument templates with placement attributes. Tunnel pool occurrences
    /// are supplied by their owning dungeon stage and accepted by `request`.
    pub fn prefab_paths(&self) -> impl ExactSizeIterator<Item = &str> {
        self.templates.prefabs.keys().map(String::as_str)
    }
    /// Verified authored data, not proof that this build's gameplay program has
    /// an execution adapter. Callers must preserve unsupported program checks.
    pub fn attributes(&self, prefab: &str) -> Result<&Attributes, String> {
        self.templates
            .prefabs
            .get(prefab)
            .and_then(|p| p.placement_attributes.as_ref())
            .ok_or_else(|| {
                "Environment prefab attributes are absent from the verified source".into()
            })
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn context(&self, request_sha256: String) -> Result<scene::Context, String> {
        if !hash(&request_sha256) {
            return Err("Invalid environment generation request identity".into());
        }
        Ok(scene::Context {
            request_sha256,
            sources_sha256: self.sha256.clone(),
        })
    }
    pub fn request(
        &self,
        context: &scene::Context,
        stage: i64,
        actors: &[Occurrence],
    ) -> Result<Request, String> {
        if context.sources_sha256 != self.sha256 {
            return Err("Environment provider source context changed".into());
        }
        request(context, stage, actors, |path| {
            self.templates.prefabs.contains_key(path)
                || self.river_environments.contains_key(path)
                || self
                    .tunnels
                    .as_ref()
                    .is_some_and(|s| s.prefabs.contains_key(path))
        })
    }
    /// Returns a prepared update, never publishes. The scene's owner/base-frame
    /// and cancellation checks remain mandatory at the final publication point.
    pub fn prepare_update_from_prefix(
        &self,
        snapshot: &scene::Snapshot,
        request: &Request,
        prefix: &scene::Prefix,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Result<Update, Verdict>, String> {
        if !validate_prefix(snapshot, request, prefix)? {
            return Ok(Err(Verdict::Incomplete));
        }
        self.prepare_update(snapshot, request, maximum_work, cancelled)
    }
    /// Preparation/index allowance; prefix construction and placement queries
    /// have their own metered work which the owning coordinator must also count.
    pub fn prepare_update(
        &self,
        snapshot: &scene::Snapshot,
        request: &Request,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Result<Update, Verdict>, String> {
        if request.context.sources_sha256 != self.sha256 {
            return Err("Environment provider source context changed".into());
        }
        prepare_resolved(
            snapshot,
            request,
            maximum_work,
            |path| {
                // Generated dungeon prefabs use the pool's complete cached
                // hierarchy capture, including inactive child volumes.
                if let Some(prefab) = self.tunnels.as_ref().and_then(|s| s.prefabs.get(path)) {
                    return prefab.registration.as_ref().map(Geometry::Cached);
                }
                if let Some(prepared) = self.river_environments.get(path) {
                    return Some(Geometry::Cached(prepared));
                }
                self.monument_geometry
                    .as_ref()
                    .unwrap_or(&self.templates)
                    .prefabs
                    .get(path)
                    .and_then(|p| p.placement_attributes.as_ref())
                    .map(Geometry::Attributes)
            },
            |record, state| self.runtime.registration(record, state).registration,
            cancelled,
        )
    }
}
impl Request {
    /// Build once before the stage's candidate trials. The winning occurrence
    /// request shares these stage inputs even though its output receipt changes.
    pub fn query_prefix(
        &self,
        snapshot: &scene::Snapshot,
        maximum_work: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Result<scene::Prefix, Verdict>, String> {
        snapshot.before(
            &self.context,
            self.stage,
            &self.inputs_sha256,
            maximum_work,
            cancelled,
        )
    }
    pub fn requirement(&self, dependencies: Vec<i64>) -> scene::Requirement {
        scene::Requirement {
            stage: self.stage,
            inputs_sha256: self.inputs_sha256.clone(),
            dependencies,
        }
    }
    pub fn inputs_sha256(&self) -> &str {
        &self.inputs_sha256
    }
    pub fn occurrences_sha256(&self) -> &str {
        &self.occurrences_sha256
    }
}
fn validate_prefix(
    snapshot: &scene::Snapshot,
    request: &Request,
    prefix: &scene::Prefix,
) -> Result<bool, String> {
    if !prefix.matches(snapshot, request.stage, request.inputs_sha256()) {
        return Err(
            "Environment query prefix is stale or belongs to another stage/document".into(),
        );
    }
    Ok(prefix.summary().declared_prefix_complete)
}
fn tag(state: Lifecycle) -> u8 {
    match state {
        Lifecycle::SerializedOnly => 0,
        Lifecycle::ActivatedAuthored => 1,
        Lifecycle::RootInactive => 2,
        Lifecycle::Removed => 3,
        Lifecycle::ActivationUnknown => 4,
        Lifecycle::Unmodeled => 5,
    }
}
fn needs_geometry(state: Lifecycle) -> bool {
    matches!(
        state,
        Lifecycle::ActivatedAuthored | Lifecycle::ActivationUnknown
    )
}
fn request(
    context: &scene::Context,
    stage: i64,
    actors: &[Occurrence],
    known: impl Fn(&str) -> bool,
) -> Result<Request, String> {
    if stage == 0
        || !hash(&context.request_sha256)
        || !hash(&context.sources_sha256)
        || actors.len() > MAX_ACTORS
        || actors
            .iter()
            .try_fold(0usize, |n, a| n.checked_add(a.prefab.len()))
            .is_none_or(|n| n > 16 * 1024 * 1024)
    {
        return Err("Invalid or excessive environment provider request".into());
    }
    // Validate before copying the occurrence/path buffers.
    for actor in actors {
        actor.pose.validate()?;
        if actor.id == 0 || actor.prefab.len() > 1024 || !known(&actor.prefab) {
            return Err("Unknown prefab or invalid environment occurrence identity".into());
        }
    }
    let publication_order = actors.iter().map(|actor| actor.id).collect();
    let mut actors = actors.to_vec();
    actors.sort_by_key(|a| a.id);
    if actors.windows(2).any(|p| p[0].id == p[1].id) {
        return Err("Duplicate environment occurrence identity".into());
    }
    let mut h = Sha256::new();
    h.update(VERSION.as_bytes());
    h.update(context.request_sha256.as_bytes());
    h.update(context.sources_sha256.as_bytes());
    h.update(stage.to_le_bytes());
    // The owning generation stage may replace a trial's outputs under the same
    // recipe/source inputs. Keep exact occurrences in a separate output receipt;
    // otherwise the immutable provider plan could never accept such a retry.
    let inputs_sha256 = format!("{:x}", h.clone().finalize());
    h.update((actors.len() as u64).to_le_bytes());
    for actor in &actors {
        h.update(actor.id.to_le_bytes());
        h.update((actor.prefab.len() as u64).to_le_bytes());
        h.update(actor.prefab.as_bytes());
        for v in actor
            .pose
            .position
            .into_iter()
            .chain(actor.pose.rotation)
            .chain(actor.pose.scale)
        {
            h.update(v.to_bits().to_le_bytes());
        }
        h.update([tag(actor.lifecycle)]);
    }
    Ok(Request {
        context: context.clone(),
        stage,
        actors,
        publication_order,
        inputs_sha256,
        occurrences_sha256: format!("{:x}", h.finalize()),
    })
}
fn charge(
    work: &mut usize,
    cost: usize,
    maximum: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<(), Verdict> {
    if cancelled() {
        return Err(Verdict::Cancelled);
    }
    let next = work.checked_add(cost).ok_or(Verdict::Budget)?;
    if next > maximum {
        return Err(Verdict::Budget);
    }
    *work = next;
    Ok(())
}
fn preparation_cost(a: &Attributes) -> Result<usize, String> {
    let e = a
        .environment
        .as_ref()
        .ok_or("Environment capture is missing; refresh monument data")?;
    let records = e.volumes.len() + e.checks.len();
    let meshes = e.collider_meshes.iter().flatten();
    let hulls = meshes.clone().filter(|m| m.convex).count();
    let raw = meshes
        .map(|m| m.vertices.len() + m.triangles.len() * 3)
        .sum::<usize>();
    let attributes = a.root_decor.len()
        + a.terrain_checks.len()
        + a.water_checks.len()
        + a.path_connectors.len()
        + a.placement_blockers.as_ref().map_or(0, Vec::len);
    // A unique prefab builds its hulls once. Per-occurrence transformation is
    // charged from the prepared geometry, without reserving construction twice.
    Ok(hulls * HULL_RESERVE + raw * 16 + (records + attributes + 1) * 128)
}
#[cfg(test)]
fn prepare_update<'a>(
    snapshot: &scene::Snapshot,
    request: &Request,
    maximum_work: usize,
    resolve: impl Fn(&str) -> Option<&'a Attributes>,
    registration: impl Fn(&EnvironmentRecord, Lifecycle) -> Registration,
    cancelled: impl Fn() -> bool,
) -> Result<Result<Update, Verdict>, String> {
    prepare_resolved(
        snapshot,
        request,
        maximum_work,
        |path| resolve(path).map(Geometry::Attributes),
        registration,
        cancelled,
    )
}
enum Geometry<'a> {
    Attributes(&'a Attributes),
    Cached(&'a Prepared),
}
enum Ready<'a> {
    Built(Prepared),
    Cached(&'a Prepared),
}
impl Ready<'_> {
    fn geometry(&self) -> &Prepared {
        match self {
            Self::Built(geometry) => geometry,
            Self::Cached(geometry) => geometry,
        }
    }
}
fn prepare_resolved<'a>(
    snapshot: &scene::Snapshot,
    request: &Request,
    maximum_work: usize,
    resolve: impl Fn(&str) -> Option<Geometry<'a>>,
    registration: impl Fn(&EnvironmentRecord, Lifecycle) -> Registration,
    cancelled: impl Fn() -> bool,
) -> Result<Result<Update, Verdict>, String> {
    if snapshot.context() != &request.context || !(1..=MAX_WORK).contains(&maximum_work) {
        return Err("Stale environment provider request or invalid work allowance".into());
    }
    let mut work = 0;
    let mut prepared = BTreeMap::new();
    let mut instances = Vec::with_capacity(request.actors.len());
    let mut volumes = 0;
    let mut unresolved = 0;
    for actor in &request.actors {
        if let Err(v) = charge(&mut work, 1, maximum_work, &cancelled) {
            return Ok(Err(v));
        }
        let mut targets = vec![];
        if actor.lifecycle == Lifecycle::Unmodeled {
            // Unknown mutation can change masks or add components even to a
            // prefab with a captured empty environment list. Do not limit this
            // uncertainty to its old authored shapes/types.
            targets.push(environment::Volume {
                key: environment::Key {
                    instance: actor.id,
                    component: UNMODELED_COMPONENT,
                },
                mask: u32::MAX,
                shape: Shape::Unresolved { bounds: None },
            });
        } else if needs_geometry(actor.lifecycle) {
            if let std::collections::btree_map::Entry::Vacant(slot) =
                prepared.entry(actor.prefab.clone())
            {
                let source =
                    resolve(&actor.prefab).ok_or("Environment placement attributes are missing")?;
                let geometry = match source {
                    Geometry::Attributes(attributes) => {
                        let cost = preparation_cost(attributes)?;
                        if let Err(v) = charge(&mut work, cost, maximum_work, &cancelled) {
                            return Ok(Err(v));
                        }
                        let Some(geometry) = Prepared::for_registration(attributes, &cancelled)
                            .map_err(|error| format!("Environment {}: {error}", actor.prefab))?
                        else {
                            return Ok(Err(Verdict::Cancelled));
                        };
                        Ready::Built(geometry)
                    }
                    Geometry::Cached(geometry) => Ready::Cached(geometry),
                };
                let transform = geometry.geometry().target_work_reservation();
                slot.insert((geometry, transform));
            }
            let (geometry, transform_cost) = &prepared[&actor.prefab];
            if let Err(v) = charge(&mut work, *transform_cost, maximum_work, &cancelled) {
                return Ok(Err(v));
            }
            let Some(t) = geometry.geometry().targets(
                actor.id,
                actor.pose,
                |r| registration(r, actor.lifecycle),
                &cancelled,
            )?
            else {
                return Ok(Err(Verdict::Cancelled));
            };
            targets = t;
        }
        if volumes + targets.len() > MAX_VOLUMES {
            return Err("Environment provider exceeds its volume allowance".into());
        }
        volumes += targets.len();
        unresolved += targets
            .iter()
            .filter(|v| matches!(v.shape, Shape::Unresolved { .. }))
            .count();
        instances.push(scene::Instance {
            id: actor.id,
            volumes: targets,
        });
    }
    if work >= maximum_work {
        return Ok(Err(Verdict::Budget));
    }
    let replacement = scene::Replacement {
        stage: request.stage,
        inputs_sha256: request.inputs_sha256.clone(),
        instances,
    };
    let update = match snapshot.prepare(
        &request.context,
        &[replacement],
        maximum_work - work,
        &cancelled,
    )? {
        Ok(update) => update,
        Err(v) => return Ok(Err(v)),
    };
    let report = Report {
        engine: VERSION,
        sources_sha256: request.context.sources_sha256.clone(),
        inputs_sha256: request.inputs_sha256.clone(),
        occurrences_sha256: request.occurrences_sha256.clone(),
        stage: request.stage,
        actors: request.actors.len(),
        volumes,
        unresolved_geometry: unresolved,
        preparation_work_reserved: work,
        index_work: update.work(),
    };
    Ok(Ok(Update { update, report }))
}

#[cfg(test)]
mod tests;
