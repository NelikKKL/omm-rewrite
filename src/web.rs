//! Слой для браузера: класс `OmmEngine` и функции-конвертеры.
//!
//! Публичный API **полностью совместим с OMM 3.x** — `omm-component.js`
//! работает без изменений. Внутри всё устроено как конвейер:
//!
//! ```text
//! parse(txt) ─▶ parser ─▶ байт-код ─▶ vm ─▶ World ─▶ vm::scene ─▶ viewer::Scene
//!                                                                      │
//! render() ◀─ viewer (WebGL2) ◀── камера (матрица вид×проекция) ◀──────┘
//! ```
#![allow(unsafe_code)] // код, сгенерированный wasm-bindgen

use std::collections::{HashMap, HashSet};

use js_sys::Array;
use wasm_bindgen::prelude::*;
use web_sys::{HtmlCanvasElement, HtmlImageElement};

use crate::bytecode::Program;
use crate::camera::{Camera, FOCAL};
use crate::convert;
use crate::parser;
use crate::viewer::gl::GlViewer;
use crate::viewer::{Scene, View};
use crate::vm::world::World;
use crate::vm::{self, anim, scene as vm_scene};

/// Прозрачность контурных линий по умолчанию (как у обводки в OMM 3.x).
const DEFAULT_OUTLINE: f32 = 0.15;

/// Вместо молчаливого `unreachable` в консоль уходит текст паники.
#[wasm_bindgen(start)]
pub fn start() {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&JsValue::from_str(&format!("OMM: внутренняя ошибка: {info}")));
    }));
}

#[wasm_bindgen]
pub struct OmmEngine {
    world: World,
    scene: Scene,
    camera: Camera,
    canvas: Option<HtmlCanvasElement>,
    viewer: Option<GlViewer>,
    /// Все когда-либо загруженные картинки (по источнику): переживают `parse`.
    images: HashMap<String, HtmlImageElement>,
    /// Источники, которые не удалось превратить в текстуру (например CORS).
    failed: HashSet<String>,
    warnings: Vec<String>,
    revision: u64,
    outline: f32,
    /// Физических пикселей холста на один логический (CSS) пиксель.
    pixel_ratio: f64,
}

impl Default for OmmEngine {
    fn default() -> Self {
        OmmEngine::new()
    }
}

#[wasm_bindgen]
impl OmmEngine {
    #[wasm_bindgen(constructor)]
    pub fn new() -> OmmEngine {
        OmmEngine {
            world: World::default(),
            scene: Scene::default(),
            camera: Camera::default(),
            canvas: None,
            viewer: None,
            images: HashMap::new(),
            failed: HashSet::new(),
            warnings: Vec::new(),
            revision: 0,
            outline: DEFAULT_OUTLINE,
            pixel_ratio: 1.0,
        }
    }

    /// Подключает холст и создаёт WebGL2-контекст. Бросает исключение, если
    /// WebGL2 недоступен или холст уже занят контекстом другого типа.
    pub fn setup(&mut self, canvas: HtmlCanvasElement) -> Result<(), JsValue> {
        let viewer = GlViewer::new(&canvas).map_err(|e| JsValue::from_str(&e))?;
        self.viewer = Some(viewer);
        self.canvas = Some(canvas);
        self.sync_viewer();
        Ok(())
    }

    pub fn resize(&self, width: u32, height: u32) {
        if let Some(canvas) = &self.canvas {
            canvas.set_width(width);
            canvas.set_height(height);
        }
    }

    /// Разбирает текст `.omm` и заменяет сцену. Камера сохраняется.
    pub fn parse(&mut self, txt: &str) {
        let program = parser::parse(txt);
        self.load_program(&program);
    }

    /// Загружает заранее скомпилированный байт-код (см. [`compile`]).
    pub fn load_bytecode(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        let program = Program::from_bytes(bytes)
            .ok_or_else(|| JsValue::from_str("байт-код повреждён или имеет неизвестный формат"))?;
        self.load_program(&program);
        Ok(())
    }

    /// Источники текстур, которые сцене нужны, но ещё не загружены.
    pub fn pending_textures(&self) -> Array {
        let out = Array::new();
        for (i, key) in self.scene.textures.iter().enumerate() {
            if self.failed.contains(key) {
                continue;
            }
            let ready = match &self.viewer {
                Some(v) => v.is_ready(i),
                None => self.images.contains_key(key),
            };
            if !ready {
                out.push(&JsValue::from_str(key));
            }
        }
        out
    }

    /// Передаёт загруженную картинку движку.
    pub fn set_texture(&mut self, src: &str, img: HtmlImageElement) {
        self.images.insert(src.to_string(), img.clone());
        self.failed.remove(src);
        let Some(viewer) = self.viewer.as_mut() else { return };
        let mut failed = false;
        for (i, key) in self.scene.textures.iter().enumerate() {
            if key.as_str() == src {
                if let Err(e) = viewer.set_texture(i, &img) {
                    self.warnings.push(e);
                    failed = true;
                }
            }
        }
        if failed {
            self.failed.insert(src.to_string());
        }
    }

    pub fn render(&mut self) {
        let (Some(canvas), Some(viewer)) = (self.canvas.as_ref(), self.viewer.as_mut()) else {
            return;
        };
        let (w, h) = (canvas.width(), canvas.height());
        // проекция считается в логических пикселях, viewport — в физических:
        // так модель имеет один и тот же размер на экранах с любым DPR
        let ratio = self.pixel_ratio;
        let vp = self.camera.view_proj_f64(f64::from(w) / ratio, f64::from(h) / ratio);
        let view = View {
            view_proj: vp.map(|v| v as f32),
            width: w,
            height: h,
            outline: self.outline,
        };
        viewer.render(&self.scene, &view);
    }

    /// Один тик анимации (`animation(...)`).
    pub fn step(&mut self) {
        if !self.world.animated {
            return;
        }
        anim::step(&mut self.world.objects);
        vm_scene::refresh(&self.world, &mut self.scene);
    }

    pub fn auto_rotate(&mut self) {
        self.camera.auto_rotate();
    }

    /// `mode == 0` — вращение, иначе — панорамирование.
    pub fn drag(&mut self, dx: f64, dy: f64, mode: u8) {
        self.camera.drag(dx, dy, mode);
    }

    pub fn zoom(&mut self, delta: f64) {
        self.camera.zoom(delta);
    }

    pub fn pinch_zoom(&mut self, initial_z: f64, initial_dist: f64, current_dist: f64) {
        self.camera.pinch_zoom(initial_z, initial_dist, current_dist);
    }

    pub fn has_animation(&self) -> bool {
        self.world.animated
    }

    pub fn focal(&self) -> f64 {
        FOCAL
    }

    pub fn camera_z(&self) -> f64 {
        self.camera.z
    }

    // ── новое в 4.0 (необязательное, старый API не затрагивает) ────────────

    /// Замечания к последнему разобранному тексту (опечатки, битые данные…).
    pub fn warnings(&self) -> Array {
        let out = Array::new();
        for w in &self.warnings {
            out.push(&JsValue::from_str(w));
        }
        out
    }

    /// Прозрачность контурных линий граней, 0..1 (0 — выключить).
    pub fn set_outline(&mut self, alpha: f32) {
        self.outline = if alpha.is_finite() { alpha.clamp(0.0, 1.0) } else { DEFAULT_OUTLINE };
    }

    /// Отношение физических пикселей холста к логическим (обычно
    /// `devicePixelRatio`). По умолчанию 1: холст имеет размер элемента, как
    /// в OMM 3.x. При 2 холст в два раза чётче, а модель выглядит так же.
    pub fn set_pixel_ratio(&mut self, ratio: f64) {
        self.pixel_ratio = if ratio.is_finite() { ratio.clamp(0.25, 8.0) } else { 1.0 };
    }
}

impl OmmEngine {
    fn load_program(&mut self, program: &Program) {
        let run = vm::run(program);
        self.revision += 1;
        self.scene = vm_scene::compose(&run.world, self.revision);
        self.world = run.world;
        self.warnings = program
            .warnings
            .iter()
            .cloned()
            .chain(run.warnings)
            .collect();
        self.sync_viewer();
    }

    /// Загружает геометрию новой сцены на GPU и возвращает на место
    /// картинки, которые были получены раньше.
    fn sync_viewer(&mut self) {
        let Some(viewer) = self.viewer.as_mut() else { return };
        viewer.sync(&self.scene);
        for (i, key) in self.scene.textures.iter().enumerate() {
            if self.failed.contains(key) {
                continue;
            }
            if let Some(img) = self.images.get(key) {
                if let Err(e) = viewer.set_texture(i, img) {
                    self.warnings.push(e);
                }
            }
        }
    }
}

/// Конвертирует текст `.obj` в текст `.omm`.
#[wasm_bindgen]
pub fn obj_to_omm(text: &str) -> Result<String, JsValue> {
    convert::obj::obj_to_omm(text).map_err(|e| JsValue::from_str(&e))
}

/// Конвертирует байты `.glb` в текст `.omm`.
#[wasm_bindgen]
pub fn glb_to_omm(data: &[u8]) -> Result<String, JsValue> {
    convert::gltf::glb_to_omm(data).map_err(|e| JsValue::from_str(&e))
}

/// Компилирует текст `.omm` в байт-код (его можно закэшировать и позже
/// передать в `OmmEngine.load_bytecode`).
#[wasm_bindgen]
pub fn compile(text: &str) -> Vec<u8> {
    parser::parse(text).to_bytes()
}

/// Версия библиотеки.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
