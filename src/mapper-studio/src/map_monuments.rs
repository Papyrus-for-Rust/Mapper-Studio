//! Accepted monument instances for map markers and restriction overlays.
use super::*;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Monument {
    pub id: u64,
    pub name: String,
    pub prefab: Option<String>,
    pub position: [f32; 3],
    pub yaw: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restriction_origin: Option<[f32; 2]>,
}

impl Monument {
    pub fn is_scenery_marker(&self) -> bool {
        self.prefab.as_deref().and_then(mapper_studio::monument_icons::rock_marker).is_some()
    }

    pub fn restriction_pose(&self) -> Option<[f32; 3]> {
        if self.is_scenery_marker() {
            return None;
        }
        let [x, z] = match self.restriction_origin {
            Some(origin) => origin,
            None if self.prefab.as_deref().is_some_and(|path| {
                path.starts_with("assets/bundled/prefabs/autospawn/tunnel-upwards/")
            }) => return None, // Old saved markers have only the surface icon position.
            None => [self.position[0], self.position[2]],
        };
        Some([x, z, self.yaw])
    }

    pub(super) fn restore_tunnel_origin(&mut self, [x, z]: [f32; 2]) {
        if self.restriction_pose().is_some() {
            return;
        }
        // Generated portals rotate only about Y. Undo the authored marker
        // offset using the same yaw convention as restriction projection.
        let (sin, cos) = self.yaw.to_radians().sin_cos();
        let origin = [
            self.position[0] - (cos * x - sin * z),
            self.position[2] - (sin * x + cos * z),
        ];
        if origin.iter().all(|value| value.abs() <= 100000.) {
            self.restriction_origin = Some(origin);
        }
    }
}

fn roadside(
    maps: &papyrus_terrain::monument_composition::Composed,
) -> Result<Vec<Monument>, String> {
    let Some(report) = maps.roads.as_ref().and_then(|r| r.surfaces.as_ref()) else {
        return Ok(Vec::new());
    };
    let stage = &report.report["staged_pipeline"]["roadside"];
    if stage.is_null() { return Ok(Vec::new()); }
    let rows = stage["placements"].as_array().ok_or("Missing roadside map instances")?;
    if rows.len() > 4096 { return Err("Too many roadside map instances".into()); }
    let mut identities = std::collections::BTreeSet::new();
    rows.iter().map(|row| {
        let id = row["request"].as_u64().ok_or("Invalid roadside map identity")?;
        let path = row["prefab"].as_str().ok_or("Missing roadside map prefab")?;
        let position: [f32; 3] = serde_json::from_value(row["position"].clone()).map_err(|e| e.to_string())?;
        let yaw = row["yaw"].as_f64().map(|v| v as f32).ok_or("Missing roadside map rotation")?;
        if !identities.insert(id) || !path.starts_with("assets/") || !path.ends_with(".prefab")
            || position.iter().any(|v| !v.is_finite() || v.abs() > 10000.) || !yaw.is_finite()
        { return Err("Invalid roadside map instance".into()); }
        Ok(Monument {
            id, name: path.rsplit('/').next().unwrap_or(path).trim_end_matches(".prefab").replace('_', " "),
            prefab: Some(path.into()), position, yaw, restriction_origin: None,
        })
    }).collect()
}

pub(super) fn generated(maps:&papyrus_terrain::monument_composition::Composed)->Result<Vec<Monument>,String> {
    let mut result=roadside(maps)?;
    if let Some(scene)=&maps.offshore {
        for instance in &scene.instances {
            let forward=papyrus_terrain::environment::rotate(instance.pose.rotation,[0.,0.,1.]);
            result.push(Monument {id:instance.id,name:instance.name.clone(),prefab:Some(instance.prefab.clone()),
                position:instance.pose.position,yaw:(-forward[0]).atan2(forward[2]).to_degrees().rem_euclid(360.),
                restriction_origin:None});
        }
    }
    if let Some(scene) = &maps.tunnels {
        for instance in &scene.instances {
            let Some(position) = instance.surface_position else { continue };
            let forward = papyrus_terrain::environment::rotate(instance.pose.rotation, [0.,0.,1.]);
            result.push(Monument { id:instance.instance, name:"Rail tunnel entrance".into(),
                prefab:Some(instance.prefab.clone()), position,
                yaw:(-forward[0]).atan2(forward[2]).to_degrees().rem_euclid(360.),
                restriction_origin:Some([instance.pose.position[0], instance.pose.position[2]]) });
        }
    }
    if let Some(scene) = &maps.objects {
        result.extend(scene.decoration.iter().flat_map(|report| &report.admitted).filter_map(|instance| {
            rock_marker(instance.transaction_pose, &instance.prefab, instance.transform.position, instance.transform.rotation)
        }));
    }
    if let Some(decor) = &maps.decoration
        && let (Some(fitting), Some(committed)) = (&decor.fitting, &decor.terrain_placement)
    {
        result.extend(committed.applied.iter().filter_map(|applied| {
            let pose = fitting.poses.get(applied.pose)?;
            let candidate = decor.candidates.get(pose.candidate)?;
            let prefab = decor.stages.get(candidate.stage)?.prefabs.get(candidate.prefab)?;
            rock_marker(applied.pose | (1 << 31), prefab, pose.position, pose.rotation)
        }));
    }
    Ok(result)
}

fn rock_marker(id: usize, prefab: &str, position: [f32; 3], rotation: [f32; 4]) -> Option<Monument> {
    let (name, _) = mapper_studio::monument_icons::rock_marker(prefab)?;
    let forward = papyrus_terrain::environment::rotate(rotation, [0., 0., 1.]);
    Some(Monument {
        id: 0x4d41_5052_0000_0000 | id as u64,
        name: name.into(),
        prefab: Some(prefab.into()),
        position,
        yaw: (-forward[0]).atan2(forward[2]).to_degrees().rem_euclid(360.),
        restriction_origin: None,
    })
}

pub(super) fn fit_scale(size:u32,monuments:&[Monument],available:f32)->f32 {
    let extent=monuments.iter().fold(size as f32*0.5,|extent,m|extent.max(m.position[0].abs()).max(m.position[2].abs()));
    if extent<=size as f32*0.5 {return 1.;}
    size as f32/(2.*extent)*(available-2.*monument_icon_panel::ICON_SIDE).max(1.)/available.max(1.)
}

pub(super) fn initial(project: &Project, solution: &papyrus_terrain::placement::Solution) -> Vec<Monument> {
    solution.outcomes.iter().filter_map(|outcome| {
        let p = outcome.position.as_ref()?;
        let target = project.targets.iter().find(|t| t.instance_id == outcome.id)?;
        Some(Monument { id: outcome.id, name: target.name.clone(), prefab: target.prefab_path.clone(),
            position: [p.x, p.y, p.z], yaw: p.yaw, restriction_origin: None })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenery_markers_keep_the_named_formation_pose_and_skip_restrictions() {
        let formations = [
            ('a', "Large God Rock"), ('b', "Anvil Rock"), ('c', ""),
            ('e', "Medium God Rock"), ('f', "Tiny God Rock"),
        ];
        let mut markers = Vec::new();
        for (i, (variant, name)) in formations.into_iter().enumerate() {
            let path = format!("assets/bundled/prefabs/autospawn/decor/v3_rock_formations_large/rock_formation_{variant}.prefab");
            let position = [22. + i as f32, 3., 17.];
            let marker = rock_marker(i, &path, position, [0., 0., 0., 1.]);
            if name.is_empty() {
                assert!(marker.is_none());
            } else {
                let marker = marker.unwrap();
                assert_eq!(marker.name, name);
                assert_eq!(marker.position, position);
                markers.push(marker);
            }
        }
        assert_eq!(markers.len(), 4);
        assert!(markers.iter().all(|m| m.is_scenery_marker() && m.restriction_pose().is_none()));
        let panel = restriction_panel::Panel::default();
        assert!(markers.iter().all(|m| !panel.shows_map_text(m.prefab.as_deref())));
        assert_eq!(panel.incomplete_count(&Project::default(), &markers), 0);
    }
    #[test]
    fn generated_tunnel_marker_uses_its_surface_frame_and_rail_icon() {
        let (hr,sr)=papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let layers=papyrus_terrain::native_layers::Layers {
            size:1000,terrain_seed:1,biome_seed:1,height_resolution:hr,surface_resolution:sr,
            height:vec![0;hr*hr],topology:vec![0;sr*sr],biome:vec![0;sr*sr*5],splat:vec![0;sr*sr*8],
            normalized_biome_cells:0,normalized_splat_cells:0,
        };
        let mut maps=papyrus_terrain::monument_composition::compose(&layers,&vec![255;sr*sr],&[],||false).unwrap().unwrap();
        let path="assets/bundled/prefabs/autospawn/tunnel-upwards/intersection-b1-n.prefab";
        maps.tunnels=Some(papyrus_terrain::tunnel_scene::Scene {
            engine:papyrus_terrain::tunnel_scene::VERSION.into(),source_sha256:"a".repeat(64),monument_sha256:"b".repeat(64),
            seed:7,layout_fingerprint:[3;32],report:serde_json::json!({}),
            instances:vec![papyrus_terrain::tunnel_scene::Instance {instance:91,prefab:path.into(),prefab_id:8,
                pose:papyrus_terrain::dungeon_base::Pose {position:[10.,-36.,20.],rotation:[0.,0.,0.,1.]},surface_position:Some([15.,8.,25.])}],
        });
        let markers=generated(&maps).unwrap();
        assert_eq!(markers.len(),1);
        assert_eq!(markers[0].id,91);
        assert_eq!(markers[0].position,[15.,8.,25.]);
        assert_eq!(markers[0].restriction_pose(),Some([10.,20.,0.]));
        let mut old_marker = serde_json::to_value(&markers[0]).unwrap();
        old_marker.as_object_mut().unwrap().remove("restriction_origin");
        let old_marker: Monument = serde_json::from_value(old_marker).unwrap();
        assert_eq!(old_marker.position,[15.,8.,25.]);
        assert!(old_marker.restriction_pose().is_none());
        assert_eq!(mapper_studio::monument_icons::default_key_for_prefab(&markers[0].name,path),Some("train-tunnel-rails"));
    }
    #[test]
    fn offshore_markers_and_fit_work_without_any_roads() {
        let (hr,sr)=papyrus_terrain::native_layers::dimensions(1000).unwrap();
        let layers=papyrus_terrain::native_layers::Layers {
            size:1000,terrain_seed:1,biome_seed:2,height_resolution:hr,surface_resolution:sr,
            height:vec![0;hr*hr],topology:vec![0;sr*sr],biome:vec![0;sr*sr*5],splat:vec![0;sr*sr*8],
            normalized_biome_cells:0,normalized_splat_cells:0,
        };
        let mut maps=papyrus_terrain::monument_composition::compose(&layers,&vec![255;sr*sr],&[],||false).unwrap().unwrap();
        maps.offshore=Some(papyrus_terrain::offshore_search::Scene {
            engine:mapper_studio::offshore_generation::VERSION.into(),source_sha256:"a".repeat(64),
            monument_sha256:"b".repeat(64),layout_fingerprint:[0;32],seed:7,stages:Vec::new(),
            instances:vec![papyrus_terrain::offshore_search::Instance {
                id:1<<62,stage:10,prefab:"assets/bundled/prefabs/autospawn/monument/offshore/oilrig_1.prefab".into(),
                name:"Large Oil Rig".into(),pose:papyrus_terrain::offshore_search::Pose {
                    position:[0.,0.,850.],rotation:[0.,0.,0.,1.],scale:[1.;3],
                },
            }],
        });
        let markers=generated(&maps).unwrap();
        assert_eq!(markers.len(),1);
        assert_eq!(markers[0].name,"Large Oil Rig");
        assert_eq!(markers[0].position,[0.,0.,850.]);
        assert_eq!(markers[0].yaw,0.);
        let scale=fit_scale(1000,&markers,800.);
        assert!(scale>0. && scale<1000./1700.);
        assert_eq!(fit_scale(1000,&[],800.),1.);
        let pixels=800.*scale;
        let top=400.-0.85*pixels;
        assert!(top+0.001>=monument_icon_panel::ICON_SIDE);
    }
    #[test]
    fn roadside_markers_use_accepted_world_poses_without_recipe_targets() {
        let ctx=egui::Context::default();
        let mut app=super::super::tests::fixture(&ctx);
        app.project.targets.clear();
        app.composition.seed_test_view(&ctx,&app.project);
        app.composition.seed_test_roadside(serde_json::json!([
            {"request":780001_u64,"prefab":"assets/bundled/prefabs/autospawn/monument/roadside/gas_station_1.prefab","position":[125.,42.,-80.],"yaw":37.},
            {"request":780002_u64,"prefab":"assets/bundled/prefabs/autospawn/monument/roadside/supermarket_1.prefab","position":[-200.,5.,250.],"yaw":280.}
        ]));
        let markers=&app.composition.view(&app.project).unwrap().generated_monuments;
        assert_eq!(markers.len(),2);
        assert_eq!(markers[0].id,780001);
        assert_eq!(markers[0].position,[125.,42.,-80.]);
        assert_eq!(markers[0].yaw,37.);
        assert!(markers[1].prefab.as_ref().unwrap().ends_with("supermarket_1.prefab"));
        assert!(app.project.targets.is_empty());
        assert!(app.retained_composition().is_none());
        assert_eq!(app.displayed_composition().unwrap().generated_monuments.len(),2);
    }
}
