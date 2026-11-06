//! Сквозные тесты публичного API: текст → байт-код → VM → viewer.

use omm_core::bytecode::Program;
use omm_core::camera::Camera;
use omm_core::convert;
use omm_core::viewer::{make_plan, Plan, View, STRIDE};
use omm_core::vm::{self, anim, scene};
use omm_core::{mesh::Mesh, parser};

/// Пример со всеми возможностями формата.
const SAMPLE: &str = "\
// Робот: группа из куба-плиты и шара
mono(
  x(0) y(0) scale(1)
  cube3 : 2
  color(80, 140, 255)
  x(0) y(-40)
  sphere3
  color(255, 200, 0)
  y(-110) scale(0.6)
)

pyramid3
x(150)
color(255, 90, 90)
animation(y0, y-60, y0)

cylinder3
x(-150) rr(45) ru(10)
ur(20) ul(10)

triangle3
z(120)

image3
texture(https://example.com/pic.png)
y(150)

mesh3
verts(0 -50 0, 50 50 0, -50 50 0)
uvs(0.5 0, 1 1, 0 1)
tris(0 1 2)
";

fn view(w: u32, h: u32, cam: &Camera) -> View {
    View { view_proj: cam.view_proj(w, h), width: w, height: h, outline: 0.15 }
}

#[test]
fn sample_has_no_warnings_anywhere() {
    let program = parser::parse(SAMPLE);
    assert!(program.warnings.is_empty(), "{:?}", program.warnings);
    let run = vm::run(&program);
    assert!(run.warnings.is_empty(), "{:?}", run.warnings);
    assert_eq!(run.world.objects.len(), 7);
    assert!(run.world.animated);
    assert_eq!(run.world.textures, vec!["https://example.com/pic.png".to_string()]);
}

#[test]
fn group_members_are_baked_and_the_rest_is_untouched() {
    let world = vm::run(&parser::parse(SAMPLE)).world;
    // identity-группа не меняет членов
    assert_eq!(world.objects[0].pos, [0.0, -40.0, 0.0]);
    assert_eq!(world.objects[0].squash, 0.5);
    assert!((world.objects[1].scale - 30.0).abs() < 1e-9); // 50 × 0.6
    assert_eq!(world.objects[2].pos[0], 150.0);
}

#[test]
fn viewer_plan_depends_only_on_texture_readiness() {
    let world = vm::run(&parser::parse(SAMPLE)).world;
    let sc = scene::compose(&world, 1);
    let v = view(800, 600, &Camera::default());
    let mut plan = Plan::default();

    // картинка не загружена: image3 скрыт, остальные 6 — непрозрачные
    make_plan(&sc, &v, &|_| false, &mut plan);
    assert_eq!(plan.opaque.len(), 6);
    assert!(plan.blended.is_empty());

    // картинка загружена: image3 уходит во второй проход
    make_plan(&sc, &v, &|t| t == 0, &mut plan);
    assert_eq!(plan.opaque.len(), 6);
    assert_eq!(plan.blended, vec![5]);
}

#[test]
fn bytecode_container_gives_the_same_world() {
    let program = parser::parse(SAMPLE);
    let restored = Program::from_bytes(&program.to_bytes()).expect("container");
    let a = vm::run(&program).world;
    let b = vm::run(&restored).world;
    assert_eq!(a.objects.len(), b.objects.len());
    assert_eq!(a.textures, b.textures);
    for (x, y) in a.objects.iter().zip(&b.objects) {
        assert_eq!(x.shape, y.shape);
        assert_eq!(x.pos, y.pos);
        assert_eq!(x.scale, y.scale);
        assert_eq!(x.rgb, y.rgb);
        assert_eq!(x.deform, y.deform);
    }
}

#[test]
fn animation_moves_instances_through_refresh() {
    let mut world = vm::run(&parser::parse(SAMPLE)).world;
    let mut sc = scene::compose(&world, 1);
    let before = sc.instances[2].model[13]; // y пирамиды
    for _ in 0..20 {
        anim::step(&mut world.objects);
    }
    scene::refresh(&world, &mut sc);
    let after = sc.instances[2].model[13];
    assert!(after < before, "пирамида должна ехать вверх (y → −60): {before} → {after}");
    // геометрия не трогается
    assert_eq!(sc.revision, 1);
}

#[test]
fn obj_import_flows_through_the_whole_pipeline() {
    let obj = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n";
    let text = convert::obj::obj_to_omm(obj).unwrap();
    let run = vm::run(&parser::parse(&text));
    assert!(run.warnings.is_empty(), "{:?}", run.warnings);
    let sc = scene::compose(&run.world, 1);
    assert_eq!(sc.instances.len(), 1);
    assert_eq!(sc.meshes[0].vertices.len(), 2 * 3 * STRIDE); // квад → 2 треугольника

    let mut plan = Plan::default();
    make_plan(&sc, &view(200, 200, &Camera::default()), &|_| false, &mut plan);
    assert_eq!(plan.opaque, vec![0]);
}

#[test]
fn scene_origin_projects_to_the_screen_centre() {
    let m = Camera::default().view_proj_f64(800.0, 600.0);
    let clip = [0usize, 1, 2, 3].map(|r| m[12 + r]); // образ начала координат
    let (nx, ny) = (clip[0] / clip[3], clip[1] / clip[3]);
    assert!(nx.abs() < 1e-12 && ny.abs() < 1e-12, "{nx} {ny}");
}

#[test]
fn old_scenes_with_typos_still_load() {
    // ошибки в тексте — это предупреждения, а не падения
    let text = "cube3\nx(abc)\nbogus(1)\ncolor(1,2)\nanimation(\nmesh3\ndata(%%%)\n)\n))\n";
    let program = parser::parse(text);
    let run = vm::run(&program);
    assert!(!program.warnings.is_empty());
    assert!(!run.world.objects.is_empty());
    let _ = scene::compose(&run.world, 1);
}

#[test]
fn mesh_data_survives_text_roundtrip_bit_for_bit() {
    let m = Mesh::new(
        vec![[0.1, 0.2, 0.3], [1.5, -2.5, 3.5], [1e-3, 1e3, -7.0]],
        Some(vec![[0.0, 0.25], [0.5, 0.75], [1.0, 0.125]]),
        vec![[0, 1, 2]],
    );
    let prim = convert::Primitive { mesh: m.clone(), color: [1, 2, 3], texture: None };
    let text = convert::primitives_to_omm(&[prim]);
    let got = vm::run(&parser::parse(&text)).world.objects[0].mesh.clone().unwrap();
    assert_eq!(got.v, m.v);
    assert_eq!(got.uv, m.uv);
    assert_eq!(got.tris, m.tris);
}
