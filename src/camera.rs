//! Камера и управление ею (вращение, панорамирование, зум) + матрица
//! «вид × проекция» для viewer.
//!
//! Формулы и коэффициенты управления — как в OMM 3.x. Нечисловые входные
//! значения (`NaN`/`±∞` из JS) игнорируются, а расстояние камеры ограничено
//! с обеих сторон, чтобы состояние никогда не «отравлялось».
//!
//! # Проекция
//!
//! В OMM 3.x точка камеры-пространства `(x, y, z)` попадала на экран так:
//!
//! ```text
//! sc = FOCAL / (FOCAL + z + camera.z)
//! screen = (x·sc + W/2 + pan_x,  y·sc + H/2 + pan_y)
//! ```
//!
//! Матрица [`Camera::view_proj`] воспроизводит это преобразование **в
//! точности** (с поправкой на ось Y вверх в NDC), поэтому картинка на GPU
//! совпадает с прежней. Дополнительно в неё заложены ближняя и дальняя
//! плоскости для буфера глубины: геометрия, оказавшаяся «за спиной»
//! камеры, корректно отсекается GPU (раньше она рисовалась зеркально).

/// Фокусное расстояние перспективной проекции, пикселей.
pub const FOCAL: f64 = 600.0;

/// Минимальное `camera.z`: ближе камера не подпускается.
pub const MIN_Z: f64 = -FOCAL + 10.0;
/// Максимальное `camera.z` (дальше смысла нет — модель меньше пикселя).
pub const MAX_Z: f64 = 1.0e6;
/// Ближняя плоскость отсечения: `FOCAL + z + camera.z ≥ NEAR_W`.
pub const NEAR_W: f64 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Расстояние (смещение по оси взгляда).
    pub z: f64,
    /// Наклон, радианы.
    pub rx: f64,
    /// Поворот вокруг вертикали, радианы.
    pub ry: f64,
    /// Панорамирование по экрану, пикселей.
    pub cx: f64,
    pub cy: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Camera { z: FOCAL, rx: 0.4, ry: 0.4, cx: 0.0, cy: 0.0 }
    }
}

impl Camera {
    fn clamp_z(&mut self) {
        self.z = self.z.clamp(MIN_Z, MAX_Z);
    }

    /// Один шаг автоповорота (`autorate`).
    pub fn auto_rotate(&mut self) {
        self.ry += 0.01;
    }

    /// Перетаскивание. `mode == 0` — вращение, иначе — панорамирование.
    pub fn drag(&mut self, dx: f64, dy: f64, mode: u8) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        if mode == 0 {
            self.ry -= dx * 0.01;
            self.rx += dy * 0.01;
        } else {
            self.cx += dx;
            self.cy += dy;
        }
    }

    /// Зум колесом (положительное значение — отдаляет).
    pub fn zoom(&mut self, delta: f64) {
        if !delta.is_finite() {
            return;
        }
        self.z += delta * 0.5;
        self.clamp_z();
    }

    /// Pinch-зум относительно начального расстояния камеры.
    pub fn pinch_zoom(&mut self, initial_z: f64, initial_dist: f64, current_dist: f64) {
        if !(initial_z.is_finite() && initial_dist.is_finite() && current_dist.is_finite()) {
            return;
        }
        if current_dist > 0.0 {
            self.z = initial_z * (initial_dist / current_dist);
            self.clamp_z();
        }
    }

    /// Матрица «вид × проекция» (column-major) для холста `width × height`.
    pub fn view_proj_f64(&self, width: f64, height: f64) -> [f64; 16] {
        let w = if width.is_finite() && width >= 1.0 { width } else { 1.0 };
        let h = if height.is_finite() && height >= 1.0 { height } else { 1.0 };

        // вращение камеры R = Ry·Rx (по строкам)
        let (s_rx, c_rx) = self.rx.sin_cos();
        let (s_ry, c_ry) = self.ry.sin_cos();
        let r = [
            [c_ry, s_ry * s_rx, s_ry * c_rx],
            [0.0, c_rx, -s_rx],
            [-s_ry, c_ry * s_rx, c_ry * c_rx],
        ];

        // d0 + z_камеры-пространства = знаменатель старой формулы
        let d0 = FOCAL + self.z;
        let far = 2.0 * d0.max(0.0) + 1.0e5;
        let a = (far + NEAR_W) / (far - NEAR_W);
        let b = -2.0 * far * NEAR_W / (far - NEAR_W);

        // строки проекции для вектора (x, y, z, 1) в системе камеры
        let p = [
            [2.0 * FOCAL / w, 0.0, 2.0 * self.cx / w, 2.0 * self.cx * d0 / w],
            [0.0, -2.0 * FOCAL / h, -2.0 * self.cy / h, -2.0 * self.cy * d0 / h],
            [0.0, 0.0, a, a * d0 + b],
            [0.0, 0.0, 1.0, d0],
        ];

        // VP = P · blockdiag(R, 1)
        let mut m = [0.0f64; 16];
        for row in 0..4 {
            for col in 0..3 {
                let mut s = 0.0;
                for k in 0..3 {
                    s += p[row][k] * r[k][col];
                }
                m[col * 4 + row] = s;
            }
            m[12 + row] = p[row][3];
        }
        m
    }

    /// То же в `f32` — формат, который принимает viewer.
    pub fn view_proj(&self, width: u32, height: u32) -> [f32; 16] {
        self.view_proj_f64(f64::from(width), f64::from(height)).map(|v| v as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_v3() {
        let c = Camera::default();
        assert_eq!((c.z, c.rx, c.ry, c.cx, c.cy), (600.0, 0.4, 0.4, 0.0, 0.0));
    }

    #[test]
    fn drag_modes() {
        let mut c = Camera::default();
        c.drag(10.0, 20.0, 0);
        assert!((c.ry - (0.4 - 0.1)).abs() < 1e-12);
        assert!((c.rx - (0.4 + 0.2)).abs() < 1e-12);
        c.drag(5.0, -3.0, 1);
        assert_eq!((c.cx, c.cy), (5.0, -3.0));
    }

    #[test]
    fn zoom_is_clamped_both_ways() {
        let mut c = Camera::default();
        c.zoom(-1.0e9);
        assert_eq!(c.z, MIN_Z);
        c.zoom(1.0e12);
        assert_eq!(c.z, MAX_Z);
        c.zoom(f64::NAN);
        assert_eq!(c.z, MAX_Z);
    }

    #[test]
    fn pinch_zoom_scales_distance() {
        let mut c = Camera::default();
        c.pinch_zoom(600.0, 100.0, 200.0); // пальцы раздвинули вдвое → ближе
        assert_eq!(c.z, 300.0);
        c.pinch_zoom(600.0, 100.0, 0.0); // деление на ноль игнорируется
        assert_eq!(c.z, 300.0);
        c.pinch_zoom(f64::INFINITY, 1.0, 1.0);
        assert_eq!(c.z, 300.0);
    }

    #[test]
    fn nan_input_never_poisons_state() {
        let mut c = Camera::default();
        c.drag(f64::NAN, 1.0, 0);
        c.drag(1.0, f64::INFINITY, 1);
        assert_eq!(c, Camera::default());
    }

    /// Проекция OMM 3.x, как она была в Canvas-рендерере.
    fn reference(cam: &Camera, p: [f64; 3], w: f64, h: f64) -> [f64; 2] {
        let (srx, crx) = cam.rx.sin_cos();
        let (sry, cry) = cam.ry.sin_cos();
        let cy = p[1] * crx - p[2] * srx;
        let cz = p[1] * srx + p[2] * crx;
        let cx = p[0] * cry + cz * sry;
        let cz = -p[0] * sry + cz * cry;
        let sc = FOCAL / (FOCAL + cz + cam.z);
        [cx * sc + w / 2.0 + cam.cx, cy * sc + h / 2.0 + cam.cy]
    }

    /// Экранные координаты и NDC-глубина через матрицу.
    fn through_matrix(m: &[f64; 16], p: [f64; 3], w: f64, h: f64) -> Option<[f64; 3]> {
        let clip = [0usize, 1, 2, 3].map(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r]);
        if clip[3] <= 0.0 {
            return None;
        }
        let (nx, ny, nz) = (clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]);
        Some([(nx * 0.5 + 0.5) * w, (0.5 - ny * 0.5) * h, nz])
    }

    #[test]
    fn view_proj_reproduces_the_v3_projection() {
        let cams = [
            Camera::default(),
            Camera { z: 450.0, rx: 0.25, ry: -1.1, cx: 12.0, cy: -7.0 },
            Camera { z: 1500.0, rx: -0.7, ry: 2.0, cx: -30.0, cy: 55.0 },
        ];
        let pts = [
            [0.0, 0.0, 0.0],
            [100.0, -50.0, 20.0],
            [-80.0, 120.0, -60.0],
            [30.0, 30.0, 30.0],
            [-200.0, -200.0, 150.0],
        ];
        for cam in &cams {
            for (w, h) in [(800.0, 600.0), (321.0, 977.0)] {
                let m = cam.view_proj_f64(w, h);
                for p in pts {
                    let want = reference(cam, p, w, h);
                    // в тестовых камерах все точки перед камерой
                    let got = through_matrix(&m, p, w, h).expect("точка перед камерой");
                    assert!((got[0] - want[0]).abs() < 1e-6, "x: {got:?} vs {want:?} cam={cam:?}");
                    assert!((got[1] - want[1]).abs() < 1e-6, "y: {got:?} vs {want:?} cam={cam:?}");
                    assert!(got[2] > -1.0 && got[2] < 1.0, "z вне диапазона: {got:?}");
                }
            }
        }
    }

    #[test]
    fn depth_grows_with_distance() {
        let cam = Camera { z: 600.0, rx: 0.0, ry: 0.0, cx: 0.0, cy: 0.0 };
        let m = cam.view_proj_f64(800.0, 600.0);
        let near = through_matrix(&m, [0.0, 0.0, -100.0], 800.0, 600.0).unwrap();
        let far = through_matrix(&m, [0.0, 0.0, 300.0], 800.0, 600.0).unwrap();
        assert!(near[2] < far[2]);
    }

    #[test]
    fn near_and_far_planes_map_to_ndc_limits() {
        let cam = Camera { z: 600.0, rx: 0.0, ry: 0.0, cx: 0.0, cy: 0.0 };
        let m = cam.view_proj_f64(800.0, 600.0);
        let d0 = FOCAL + cam.z;
        let far = 2.0 * d0 + 1.0e5;
        let at_near = through_matrix(&m, [0.0, 0.0, NEAR_W - d0], 800.0, 600.0).unwrap();
        let at_far = through_matrix(&m, [0.0, 0.0, far - d0], 800.0, 600.0).unwrap();
        assert!((at_near[2] + 1.0).abs() < 1e-9, "{at_near:?}");
        assert!((at_far[2] - 1.0).abs() < 1e-9, "{at_far:?}");
    }

    #[test]
    fn points_behind_the_camera_have_non_positive_w() {
        let cam = Camera { z: 600.0, rx: 0.0, ry: 0.0, cx: 0.0, cy: 0.0 };
        let m = cam.view_proj_f64(800.0, 600.0);
        // w = 1200 + z ≤ 0 → z ≤ −1200
        assert!(through_matrix(&m, [0.0, 0.0, -1300.0], 800.0, 600.0).is_none());
    }

    #[test]
    fn degenerate_canvas_sizes_do_not_produce_nan() {
        for (w, h) in [(0, 0), (1, 1), (0, 600)] {
            let m = Camera::default().view_proj(w, h);
            assert!(m.iter().all(|v| v.is_finite()), "{w}x{h}");
        }
    }
}
