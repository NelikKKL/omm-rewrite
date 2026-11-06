//! # OMM — ультралёгкий 3D-движок
//!
//! Текстовый формат `.omm`, WebGL2-рендер, импорт OBJ/GLB. Ядро на Rust,
//! публичный API для браузера — через WASM (`OmmEngine`).
//!
//! # Архитектура
//!
//! ```text
//!  текст .omm ──▶ parser ──▶ байт-код ──▶ vm ──▶ данные ──▶ viewer ──▶ WebGL2
//!                  │           │          │                    │
//!   только синтаксис     контракт    строит мир        ничего не знает
//!   (ничего не строит)  (bytecode)  (объекты, группы,  о формате и VM:
//!                                    меши, анимация)   рисует треугольники
//! ```
//!
//! * [`parser`] — читает текст и эмитит байт-код. Не знает про сцену.
//! * [`bytecode`] — набор инструкций и контейнер `OMMB`.
//! * [`vm`] — исполняет байт-код, строит мир и отдаёт viewer'у простые данные.
//! * [`viewer`] — рисует сетки и экземпляры; зависит только от собственных типов.
//! * [`camera`] — состояние камеры, управление и матрица «вид × проекция».
//! * [`convert`] — импорт `.obj` / `.glb` в текст `.omm`.
//!
//! Всё, кроме тонкого слоя `web` и WebGL-бэкенда, **не имеет зависимостей**
//! и тестируется обычным `cargo test`.
//!
//! # Пример (без браузера)
//!
//! ```
//! use omm_core::{parser, vm};
//!
//! let program = parser::parse("cube3\nx(10)\ncolor(80, 140, 255)\n");
//! let run = vm::run(&program);
//! assert_eq!(run.world.objects.len(), 1);
//! assert_eq!(run.world.objects[0].pos[0], 10.0);
//!
//! let scene = vm::scene::compose(&run.world, 1); // данные для viewer
//! assert_eq!(scene.instances.len(), 1);
//! ```

#![deny(unsafe_code)]

pub mod base64;
pub mod bytecode;
mod bytes;
pub mod camera;
pub mod convert;
pub mod json;
pub mod mesh;
pub mod parser;
pub mod viewer;
pub mod vm;

#[cfg(target_arch = "wasm32")]
mod web;
