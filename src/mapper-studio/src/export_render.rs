use super::*;
use mapper_studio::image_render::{Markers, Metadata, REFERENCE_SIDE, Settings};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub(super) struct Scene {
    pub project: Project,
    pub monuments: Vec<map_monuments::Monument>,
    pub restrictions: restriction_panel::Panel,
    pub icons: monument_icon_panel::Panel,
    pub font: Option<Arc<mapper_studio::map_labels::Prepared>>,
    pub preferences: mapper_studio::preferences::Preferences,
    pub settings_path: Option<PathBuf>,
    pub background: Color32,
}
impl Scene {
    pub fn draw(
        mut self,
        source: export_panel::Source,
        resolution: usize,
        settings: Settings,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<(egui::ColorImage, Option<Metadata>)>, String> {
        if cancelled() {
            return Ok(None);
        }
        let scale = resolution as f32 / REFERENCE_SIDE;
        let context = egui::Context::default();
        context.set_pixels_per_point(scale);
        let mut textures = egui::TexturesDelta::default();
        let mut extent = self.project.size as f32 / 2.;
        if settings.markers == Markers::Text {
            // Initialize the fallback fonts before measuring ink. The same
            // layouts are reused when the overlay is painted below.
            textures = context.run_ui(egui::RawInput::default(), |_| {}).textures_delta;
            for monument in &self.monuments {
                if !self.restrictions.shows_map_text(monument.prefab.as_deref()) {
                    continue;
                }
                let ink = self.icons.labels.bounds(&context, self.font.as_ref(), &monument.name);
                for (offset, dimension) in [monument.position[0], monument.position[2]]
                    .into_iter().zip([ink.width(), ink.height()])
                {
                    // Leave room for the antialiased edge at the output scale.
                    let span = (1. - (dimension + 4. / scale) / REFERENCE_SIDE).max(0.25);
                    extent = extent.max(offset.abs() / span);
                }
            }
        } else if settings.markers != Markers::None {
            for monument in &self.monuments {
                let offset = monument.position[0].abs().max(monument.position[2].abs());
                extent = extent.max(offset / (1. - 36. / REFERENCE_SIDE));
            }
        }
        if settings.no_build {
            extent = extent.max(self.restrictions.extent(&self.monuments) / (1. - 36. / REFERENCE_SIDE));
        }
        let side = REFERENCE_SIDE * self.project.size as f32 / (2. * extent);
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::splat(REFERENCE_SIDE));
        let map = Rect::from_center_size(canvas.center(), Vec2::splat(side));
        let pixel_map = Rect::from_min_max(map.min * scale, map.max * scale);
        let font_bytes = if settings.markers == Markers::Text {
            self.font.as_ref().map_or(0, |f| f.image.pixels.len() * 4)
        } else {
            0
        };
        let grid_bytes = if settings.grid {
            self.font.as_ref().and_then(|font| font.grid.as_ref())
                .map_or(0, |style| style.atlas.pixels.len() * 4)
        } else {
            0
        };
        let overlays = settings.markers != Markers::None || settings.no_build || settings.grid;
        let icon_bytes = if settings.markers == Markers::Icons {
            self.icons
                .export_texture_bytes(&self.monuments, &self.preferences)
        } else {
            0
        };
        let required = resolution * resolution * if settings.terrain && overlays { 8 } else { 4 }
            + font_bytes
            + grid_bytes
            + 2 * 1024 * 1024
            + icon_bytes;
        let required = required
            + if settings.no_build {
                self.restrictions.export_bytes(&self.monuments)
            } else {
                0
            };
        if required > mapper_studio::generation_resources::current().image_memory_mib * 1024 * 1024
        {
            return Err("Image overlays exceed the image memory limit in Settings".into());
        }
        let mut base = if settings.terrain {
            source.render(resolution, &cancelled)?
        } else {
            None
        };
        if cancelled() {
            return Ok(None);
        }
        if settings.terrain && base.is_none() {
            return Ok(None);
        }
        let mut metadata =
            Metadata::new(&self.project, [resolution; 2], pixel_map, settings.clone());
        for monument in &self.monuments {
            metadata.add(
                monument.id,
                monument.name.clone(),
                monument.prefab.clone(),
                monument.position,
                monument.yaw,
            )?;
        }
        if map == canvas
            && settings.terrain
            && settings.markers == Markers::None
            && !settings.no_build
            && !settings.grid
        {
            return Ok(base
                .take()
                .map(|image| (image, settings.monument_data.then_some(metadata))));
        }
        if !settings.terrain && !overlays {
            return Ok(Some((
                egui::ColorImage::filled([resolution; 2], Color32::TRANSPARENT),
                settings.monument_data.then_some(metadata),
            )));
        }
        let mut image = egui::ColorImage::filled(
            [resolution; 2],
            if settings.terrain {
                self.background
            } else {
                Color32::TRANSPARENT
            },
        );
        self.preferences.monument_markers = match settings.markers {
            Markers::Icons => mapper_studio::map_labels::Mode::Icons,
            Markers::Text => mapper_studio::map_labels::Mode::Text,
            _ => mapper_studio::map_labels::Mode::None,
        };
        if settings.markers == Markers::Text
            && scale != 1.
            && let Some(font) = &self.font
        {
            let image = Arc::new(font.style.image_scaled(&font.alpha, scale)?);
            let image_sha256 = format!("{:x}", Sha256::digest(bytemuck::cast_slice(&image.pixels)));
            self.font = Some(Arc::new(mapper_studio::map_labels::Prepared {
                metadata:font.metadata.clone(),
                installation: font.installation.clone(),
                sha256: font.sha256.clone(),
                style: font.style.clone(),
                alpha: font.alpha.clone(),
                image,
                image_sha256,
                grid: font.grid.clone(),
            }));
        }
        let base_texture = base
            .map(|base| context.load_texture("export-terrain", base, egui::TextureOptions::LINEAR));
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(canvas),
                ..Default::default()
            },
            |ui| {
                let painter = ui.painter().with_clip_rect(canvas);
                if let Some(texture) = &base_texture {
                    painter.image(
                        texture.id(),
                        map,
                        Rect::from_min_max(Pos2::ZERO, egui::pos2(1., 1.)),
                        Color32::WHITE,
                    );
                }
                if settings.no_build {
                    let (_, missing) = self.restrictions.paint_export(
                        &painter,
                        map,
                        canvas,
                        self.project.size as f32,
                        &self.monuments,
                    );
                    metadata.no_build_complete = Some(missing == 0);
                }
                if settings.grid {
                    restriction_panel::grid(&painter, map, canvas, self.project.size,
                        self.font.as_ref().and_then(|font| font.grid.as_deref()));
                }
                if settings.markers == Markers::Text {
                    for (monument, data) in self.monuments.iter().zip(&mut metadata.monuments) {
                        if cancelled() {
                            return;
                        }
                        if !self.restrictions.shows_map_text(monument.prefab.as_deref()) {
                            continue;
                        }
                        let [x, y] = data.pixel_center;
                        let center = egui::pos2(x / scale, y / scale);
                        self.icons.labels.paint(&painter, center, self.font.as_ref(), &monument.name);
                        let rect = self.icons.labels.bounds(&context, self.font.as_ref(), &monument.name)
                            .translate(center.to_vec2());
                        data.label_pixel_bounds = Some([
                            rect.left() * scale,
                            rect.top() * scale,
                            rect.right() * scale,
                            rect.bottom() * scale,
                        ]);
                    }
                    return;
                }
                for monument in &self.monuments {
                    if settings.markers == Markers::None {
                        break;
                    }
                    let point = egui::pos2(
                        map.left()
                            + (monument.position[0] / self.project.size as f32 + 0.5) * map.width(),
                        map.top()
                            + (0.5 - monument.position[2] / self.project.size as f32)
                                * map.height(),
                    );
                    if settings.markers == Markers::Centers {
                        painter.circle_filled(point, 3.5, Color32::WHITE);
                        painter.circle_stroke(point, 3.5, Stroke::new(1., Color32::BLACK));
                        painter.line_segment(
                            [point - egui::vec2(6., 0.), point + egui::vec2(6., 0.)],
                            Stroke::new(1., Color32::BLACK),
                        );
                        painter.line_segment(
                            [point - egui::vec2(0., 6.), point + egui::vec2(0., 6.)],
                            Stroke::new(1., Color32::BLACK),
                        );
                    } else if !self.icons.paint(
                        &painter,
                        point,
                        monument.prefab.as_deref(),
                        &self.preferences,
                        self.settings_path.as_deref(),
                        map_label_panel::Text {
                            name: &monument.name,
                            source: self.font.as_ref(),
                        },
                    ) {
                        painter.circle_filled(
                            point,
                            monument_icon_panel::PLACED_RADIUS,
                            Color32::from_rgb(118, 207, 245),
                        );
                    }
                }
            },
        );
        textures.append(std::mem::take(&mut output.textures_delta));
        output.textures_delta = textures;
        if cancelled() {
            output.textures_delta.clear();
            return Ok(None);
        }
        if !mapper_studio::image_raster::paint(&mut image, &context, output, scale, cancelled)? {
            return Ok(None);
        }
        Ok(Some((image, settings.monument_data.then_some(metadata))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_text_uses_centered_ink_and_transparent_background() {
        check_text_export(Project::default(), None);
    }

    #[test]
    #[ignore = "Requires a local Rust client font capture"]
    fn exported_client_text_uses_centered_ink_and_transparent_background() {
        let folder = PathBuf::from(std::env::var_os("MAPPER_TEST_MAP_LABELS").unwrap());
        let project = Project::load(&PathBuf::from(
            std::env::var_os("MAPPER_TEST_MAP_PROJECT").unwrap(),
        ))
        .unwrap();
        let metadata = serde_json::from_slice(&std::fs::read(folder.join("labels.json")).unwrap()).unwrap();
        let style = mapper_studio::map_labels::Style::decode(&metadata).unwrap();
        let alpha: Arc<[u8]> = std::fs::read(folder.join("map-font.atlas")).unwrap().into();
        let font = Arc::new(mapper_studio::map_labels::Prepared {
            metadata: Arc::new(metadata),
            installation: project.installation.clone().unwrap(),
            sha256: "owned-font".into(),
            image: Arc::new(style.image(&alpha).unwrap()),
            style,
            alpha,
            image_sha256: "owned-font-image".into(),
            grid: None,
        });
        check_text_export(project, Some(font));
    }

    fn check_text_export(project: Project, font: Option<Arc<mapper_studio::map_labels::Prepared>>) {
        for resolution in [256, 1024] {
            let (image, metadata) = Scene {
                project: project.clone(),
                monuments: vec![map_monuments::Monument {
                    id: 1,
                    name: "ABANDONED MILITARY BASE\nRESEARCH FACILITY".into(),
                    prefab: None,
                    position: [-2600., 6., -200.],
                    yaw: 0.,
                    restriction_origin: None,
                }],
                restrictions: Default::default(),
                icons: Default::default(),
                font: font.clone(),
                preferences: Default::default(),
                settings_path: None,
                background: Color32::WHITE,
            }
            .draw(
                export_panel::Source::Unavailable,
                resolution,
                Settings { markers: Markers::Text, terrain: false, ..Default::default() },
                || false,
            )
            .unwrap().unwrap();
            let metadata = metadata.unwrap();
            let marker = &metadata.monuments[0];
            let [x, y] = marker.pixel_center;
            let [left, top, right, bottom] = marker.label_pixel_bounds.unwrap();
            assert!(left >= 0. && right <= resolution as f32);
            assert!(((left + right) / 2. - x).abs() < 0.001);
            assert!(((top + bottom) / 2. - y).abs() < 0.001);
            assert!(right - left > 5. * (bottom - top));
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("text.png");
            assert!(mapper_studio::image_export::write_png(&path, &image, || false).unwrap());
            let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
            let mut reader = decoder.read_info().unwrap();
            let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
            let info = reader.next_frame(&mut pixels).unwrap();
            let mut ink = Rect::NOTHING;
            for (index, pixel) in pixels[..info.buffer_size()].chunks_exact(4).enumerate() {
                if pixel[3] == 0 { continue; }
                let point = egui::pos2((index % resolution) as f32 + 0.5, (index / resolution) as f32 + 0.5);
                assert!(Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom))
                    .expand(2.).contains(point), "Label leaked at {point:?}");
                assert!(pixel[..3].iter().all(|&channel| channel < 96), "Unexpected light label pixel {pixel:?}");
                if pixel[3] > 60 { ink.extend_with(point); }
            }
            assert!(ink.is_positive());
            assert!(ink.center().distance(egui::pos2(x, y)) <= 1.5, "{ink:?}, expected {x},{y}");
        }
    }

    #[test]
    #[ignore = "Requires a local generated world bundle and its matching Rust installation"]
    fn render_saved_native_world_with_monument_overlays() {
        use papyrus_terrain::{
            monument_composition::Composed, native_layers::Layers, ocean::Ocean,
        };
        let bundle = PathBuf::from(
            std::env::var_os("MAPPER_TEST_IMAGE_BUNDLE").expect("MAPPER_TEST_IMAGE_BUNDLE"),
        );
        let root = PathBuf::from(
            std::env::var_os("MAPPER_TEST_RUST_ROOT").expect("MAPPER_TEST_RUST_ROOT"),
        );
        let output = PathBuf::from(
            std::env::var_os("MAPPER_TEST_IMAGE_OUTPUT").expect("MAPPER_TEST_IMAGE_OUTPUT"),
        );
        std::fs::create_dir_all(&output).unwrap();
        let json = |name: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(bundle.join(name)).unwrap()).unwrap()
        };
        let read = |name: &str| std::fs::read(bundle.join(name)).unwrap();
        let project = Project::load(&bundle.join("project.mapper")).unwrap();
        let (hr, sr) = papyrus_terrain::native_layers::dimensions(project.size).unwrap();
        let height = read("height.i16le")
            .chunks_exact(2)
            .map(|v| i16::from_le_bytes(v.try_into().unwrap()))
            .collect();
        let topology = read("topology.i32le")
            .chunks_exact(4)
            .map(|v| i32::from_le_bytes(v.try_into().unwrap()))
            .collect();
        let layers = Layers {
            size: project.size,
            terrain_seed: project.seeds[0].value,
            biome_seed: project.seeds[1].value,
            height_resolution: hr,
            surface_resolution: sr,
            height,
            topology,
            biome: read("biome.u8"),
            splat: read("splat.u8"),
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let local_water = read("local-water.i16le")
            .chunks_exact(2)
            .map(|v| i16::from_le_bytes(v.try_into().unwrap()))
            .collect();
        let classification = read("ocean-classification.u8");
        let ocean = Ocean {
            resolution: sr,
            classification,
            topology: layers.topology.clone(),
            local_water,
            ocean_cells: 0,
            enclosed_cells: 0,
            changed_topology_cells: 0,
        };
        let height = read("gameplay-height.i16le")
            .chunks_exact(2)
            .map(|v| i16::from_le_bytes(v.try_into().unwrap()))
            .collect();
        let gameplay_height = Some(papyrus_terrain::gameplay_height::Output {
            height,
            report: papyrus_terrain::gameplay_height::Report {
                engine: papyrus_terrain::gameplay_height::VERSION,
                contributors: 0,
                changed_samples: 0,
                work: 0,
                physics_cooking_validated: false,
            },
        });
        let maps = Arc::new(Composed {
            custom_baked: Vec::new(),
            layers,
            alpha: read("alpha.u8"),
            ocean: Some(ocean),
            gameplay_height,
            rivers: None,
            rails: None,
            objects: None,
            dungeons: None,
            tunnels: bundle.join("tunnels.json").is_file().then(|| {
                serde_json::from_value(json("tunnels.json")).unwrap()
            }),
            offshore: None,
            clutter_topology: None,
            cliff_topology: None,
            topology_before_overrides: None,
            topology_overrides: None,
            spawn_audit: None,
            decoration: None,
            roads: None,
            clamped_height_samples: 0,
        });
        let snapshot = mapper_studio::image_snapshot::Snapshot::capture(&maps, || false)
            .unwrap()
            .unwrap();
        let installation = project.installation.as_ref().unwrap();
        let palette =
            mapper_studio::terrain_palette::Palette::discover(&root, installation, || false)
                .unwrap()
                .map(Arc::new);
        let data_folder = installation.native.relative_path.split('/').next().unwrap();
        let has_restrictions_source = root.join(data_folder).join("globalgamemanagers").is_file();
        let catalog = Arc::new(mapper_studio::no_build::prepare_catalog(
            &root, installation, has_restrictions_source, || false,
        ).unwrap());
        let font = mapper_studio::map_labels::prepare(&root, installation, || false)
            .unwrap()
            .map(Arc::new);
        assert!(palette.is_some() && font.is_some());
        // Exercise the production portal marker conversion with the retained
        // underground placements. The map icon and collider use separate origins.
        let mut monuments = map_monuments::generated(&maps).unwrap();
        if let Some(tunnels) = &maps.tunnels {
            let surface_portals = tunnels
                .instances
                .iter()
                .filter(|instance| instance.surface_position.is_some());
            for portal in surface_portals {
                let marker = monuments.iter().find(|m| m.id == portal.instance).unwrap();
                assert_eq!(marker.position, portal.surface_position.unwrap());
                assert_eq!(
                    marker.restriction_origin,
                    Some([portal.pose.position[0], portal.pose.position[2]])
                );
                if has_restrictions_source {
                    let report = catalog.entries[&portal.prefab]
                        .no_build_geometry
                        .as_ref()
                        .unwrap();
                    assert!(report.complete, "Incomplete portal coverage: {}", portal.prefab);
                }
            }
        }
        for row in json("layout.json")["outcomes"].as_array().unwrap() {
            let id = row["id"].as_u64().unwrap();
            let p = &row["position"];
            if p.is_null() {
                continue;
            }
            let target = project
                .targets
                .iter()
                .find(|t| t.instance_id == id)
                .unwrap();
            monuments.push(map_monuments::Monument {
                id,
                name: target.name.clone(),
                prefab: target.prefab_path.clone(),
                position: ["x", "y", "z"].map(|key| p[key].as_f64().unwrap() as f32),
                yaw: p["yaw"].as_f64().unwrap() as f32,
                restriction_origin: None,
            });
        }
        for row in json("roads.json")["surfaces"]["staged_pipeline"]["roadside"]["placements"]
            .as_array()
            .unwrap()
        {
            monuments.push(map_monuments::Monument {
                id: row["request"].as_u64().unwrap(),
                name: row["prefab"].as_str().unwrap().into(),
                prefab: Some(row["prefab"].as_str().unwrap().into()),
                position: serde_json::from_value(row["position"].clone()).unwrap(),
                yaw: row["yaw"].as_f64().unwrap() as f32,
                restriction_origin: None,
            });
        }
        for row in json("offshore.json")["instances"].as_array().unwrap() {
            let rotation = serde_json::from_value(row["pose"]["rotation"].clone()).unwrap();
            let forward = papyrus_terrain::environment::rotate(rotation, [0., 0., 1.]);
            monuments.push(map_monuments::Monument {
                id: row["id"].as_u64().unwrap(),
                name: row["name"].as_str().unwrap().into(),
                prefab: Some(row["prefab"].as_str().unwrap().into()),
                position: serde_json::from_value(row["pose"]["position"].clone()).unwrap(),
                yaw: (-forward[0])
                    .atan2(forward[2])
                    .to_degrees()
                    .rem_euclid(360.),
                restriction_origin: None,
            });
        }
        for monument in &mut monuments {
            if let Some(name) = monument
                .prefab
                .as_ref()
                .and_then(|p| catalog.entries.get(p)?.display_name.as_ref())
            {
                monument.name = name.clone();
            }
        }
        let mut reports = Vec::new();
        for (name, resolution, settings) in [
            (
                "icons",
                2048,
                Settings {
                    markers: Markers::Icons,
                    grid: true,
                    no_build: true,
                    ..Default::default()
                },
            ),
            (
                "text",
                2048,
                Settings {
                    markers: Markers::Text,
                    ..Default::default()
                },
            ),
            (
                "centers",
                1024,
                Settings {
                    terrain: false,
                    markers: Markers::Centers,
                    ..Default::default()
                },
            ),
        ] {
            let mut restrictions = restriction_panel::Panel::default();
            restrictions.seed(catalog.clone());
            let coverage = settings.no_build.then(|| {
                restrictions.incomplete_count(&project, &monuments) == 0
            });
            let scene = Scene {
                project: project.clone(),
                monuments: monuments.clone(),
                restrictions,
                icons: monument_icon_panel::Panel::with_export_catalog(Some(catalog.clone())),
                font: font.clone(),
                preferences: Default::default(),
                settings_path: None,
                background: ocean_background(palette.as_deref(), &Default::default()),
            };
            let start = std::time::Instant::now();
            let (image, data) = scene
                .draw(
                    export_panel::Source::Saved {
                        snapshot: snapshot.clone(),
                        palette: palette.clone(),
                    },
                    resolution,
                    settings,
                    || false,
                )
                .unwrap()
                .unwrap();
            let data = data.unwrap();
            assert_eq!(data.monuments.len(), monuments.len());
            assert!(data.monuments.iter().all(|m| m.inside_image));
            assert_eq!(data.no_build_complete, coverage);
            if coverage.is_some() && !has_restrictions_source {
                assert_eq!(data.no_build_complete, Some(false));
            }
            mapper_studio::image_export::write_png_with_metadata(
                &output.join(format!("native-{name}.png")),
                &image,
                Some(&data),
                || false,
            )
            .unwrap();
            reports.push(serde_json::json!({"mode":name,"resolution":resolution,"monuments":monuments.len(),"seconds":start.elapsed().as_secs_f64(),"no_build_complete":data.no_build_complete}));
        }
        std::fs::write(
            output.join("render-check.json"),
            serde_json::to_vec_pretty(&reports).unwrap(),
        )
        .unwrap();
    }
    fn scene() -> Scene {
        Scene {
            project: Project::default(),
            monuments: vec![map_monuments::Monument {
                id: 1,
                name: "Airfield".into(),
                prefab: Some(
                    "assets/bundled/prefabs/autospawn/monument/large/airfield_1.prefab".into(),
                ),
                position: [0., 0., 0.],
                yaw: 0.,
                restriction_origin: None,
            }],
            restrictions: Default::default(),
            icons: Default::default(),
            font: None,
            preferences: Default::default(),
            settings_path: None,
            background: MAP_BACKGROUND,
        }
    }
    #[test]
    fn transparent_centers_export_without_reading_terrain_and_keep_pixel_coordinates() {
        let settings = Settings {
            terrain: false,
            markers: Markers::Centers,
            ..Default::default()
        };
        let (image, data) = scene()
            .draw(export_panel::Source::Unavailable, 256, settings, || false)
            .unwrap()
            .unwrap();
        assert_eq!(image.size, [256, 256]);
        assert_eq!(image.pixels[0], Color32::TRANSPARENT);
        assert!(image.pixels[128 * 256 + 128].a() > 0);
        assert_eq!(data.unwrap().monuments[0].pixel_center, [128., 128.]);
    }
    #[test]
    fn clean_export_keeps_original_pixels_and_data_can_be_omitted() {
        let original = egui::ColorImage::filled([32, 32], Color32::BLUE);
        let (image, data) = scene()
            .draw(
                export_panel::Source::Legacy(std::sync::Arc::new(original.clone())),
                32,
                Settings {
                    monument_data: false,
                    ..Default::default()
                },
                || false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(image.pixels, original.pixels);
        assert!(data.is_none());
    }
    #[test]
    fn icons_and_fallback_text_are_visible_and_exports_are_cancellable() {
        for mode in [Markers::Icons, Markers::Text] {
            let (image, _) = scene()
                .draw(
                    export_panel::Source::Unavailable,
                    256,
                    Settings {
                        terrain: false,
                        markers: mode,
                        ..Default::default()
                    },
                    || false,
                )
                .unwrap()
                .unwrap();
            assert!(image.pixels.iter().any(|p| p.a() > 0));
            assert_eq!(image.pixels[0], Color32::TRANSPARENT);
        }
        assert!(
            scene()
                .draw(
                    export_panel::Source::Unavailable,
                    256,
                    Settings::default(),
                    || true
                )
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn text_export_centers_overlapping_names_without_white_halos() {
        let mut scene = scene();
        let mut nearby = scene.monuments[0].clone();
        nearby.id += 1;
        nearby.name = "Water Treatment Plant".into();
        nearby.position[0] += 5.;
        scene.monuments.push(nearby);
        let (image, data) = scene.draw(
            export_panel::Source::Unavailable,
            1024,
            Settings { terrain: false, markers: Markers::Text, ..Default::default() },
            || false,
        ).unwrap().unwrap();
        let data = data.unwrap();
        for monument in &data.monuments {
            let [left, top, right, bottom] = monument.label_pixel_bounds.unwrap();
            let center = [(left + right) / 2., (top + bottom) / 2.];
            assert!((center[0] - monument.pixel_center[0]).abs() < 0.001);
            assert!((center[1] - monument.pixel_center[1]).abs() < 0.001);
        }
        assert!(image.pixels.iter().any(|p| p.a() != 0));
        assert!(image.pixels.iter().all(|p| p.r() <= 43 && p.g() <= 43 && p.b() <= 43));
    }

    #[test]
    fn text_export_preserves_terrain_between_glyphs() {
        let scene = scene();
        let terrain = Color32::from_rgb(33, 83, 102);
        let (image, data) = scene.draw(
            export_panel::Source::Legacy(Arc::new(egui::ColorImage::filled([16; 2], terrain))),
            1024,
            Settings { markers: Markers::Text, ..Default::default() },
            || false,
        ).unwrap().unwrap();
        let data = data.unwrap();
        let [left, top, right, bottom] = data.monuments[0].label_pixel_bounds.unwrap();
        let mut unchanged = 0;
        let mut ink = 0;
        for y in top.ceil() as usize..bottom.floor() as usize {
            for x in left.ceil() as usize..right.floor() as usize {
                let pixel = image[(x, y)];
                if pixel == terrain {
                    unchanged += 1;
                } else {
                    assert_eq!(pixel.a(), 255);
                    assert!(pixel.r() <= 43 && pixel.g() <= terrain.g() && pixel.b() <= terrain.b());
                    ink += 1;
                }
            }
        }
        assert!(unchanged > 0 && ink > 0);
    }
    #[test]
    fn offshore_centers_fit_inside_the_export_canvas() {
        let mut scene = scene();
        scene.monuments[0].position = [0., 0., scene.project.size as f32];
        let (image, data) = scene
            .draw(
                export_panel::Source::Unavailable,
                256,
                Settings {
                    terrain: false,
                    markers: Markers::Centers,
                    ..Default::default()
                },
                || false,
            )
            .unwrap()
            .unwrap();
        let point = &data.unwrap().monuments[0];
        assert!(point.inside_image);
        assert!(point.pixel_center[1] > 0. && point.pixel_center[1] < 128.);
        assert!(image.pixels.iter().any(|p| p.a() > 0));
    }
}
