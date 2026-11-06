//! Покадровая анимация объектов (`animation(...)`).
//!
//! Один вызов [`step`] = один тик: каждый анимированный объект делает шаг
//! к текущему ключевому кадру (до `speed` пикселей по позиции и
//! `ROT_SPEED` радиан по каждому углу). Когда цель достигнута, объект
//! переходит к следующему кадру, а после последнего — снова к первому.

use std::f64::consts::PI;

use super::world::Object;

/// Максимальный шаг по углу за тик, радианы.
pub const ROT_SPEED: f64 = 0.05;

/// Продвигает анимацию всех объектов на один тик.
pub fn step(objects: &mut [Object]) {
    for obj in objects.iter_mut() {
        step_object(obj);
    }
}

fn step_object(o: &mut Object) {
    let (frame, speed, len) = match o.anim.as_ref() {
        Some(a) if !a.frames.is_empty() => {
            (a.frames[a.index % a.frames.len()], a.speed, a.frames.len())
        }
        _ => return,
    };

    let tx = frame.x.unwrap_or(o.pos[0]);
    let ty = frame.y.unwrap_or(o.pos[1]);
    let tz = frame.z.unwrap_or(o.pos[2]);
    let trx = frame.ru.map_or(o.rot_x, |v| v * PI / 180.0);
    let try_ = frame.rr.map_or(o.rot_y, |v| v * PI / 180.0);

    let dx = tx - o.pos[0];
    let dy = ty - o.pos[1];
    let dz = tz - o.pos[2];
    let dist = (dx * dx + dy * dy + dz * dz).sqrt();

    if dist > speed {
        let k = speed / dist;
        o.pos[0] += dx * k;
        o.pos[1] += dy * k;
        o.pos[2] += dz * k;
    } else {
        o.pos = [tx, ty, tz];
    }

    let drx = trx - o.rot_x;
    let dry = try_ - o.rot_y;
    o.rot_x = if drx.abs() > ROT_SPEED { o.rot_x + drx.signum() * ROT_SPEED } else { trx };
    o.rot_y = if dry.abs() > ROT_SPEED { o.rot_y + dry.signum() * ROT_SPEED } else { try_ };

    let arrived = dist <= speed && drx.abs() <= ROT_SPEED && dry.abs() <= ROT_SPEED;
    if arrived {
        if let Some(a) = o.anim.as_mut() {
            a.index = (a.index + 1) % len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::world::{Animation, Keyframe, Shape};
    use super::*;

    fn animated(frames: Vec<Keyframe>) -> Object {
        let mut o = Object::new(Shape::Cube);
        o.anim = Some(Animation { frames, index: 0, speed: 2.0 });
        o
    }

    #[test]
    fn moves_at_constant_speed_then_snaps() {
        let kf = Keyframe { y: Some(5.0), ..Keyframe::default() };
        let mut objs = vec![animated(vec![kf])];
        step(&mut objs);
        assert!((objs[0].pos[1] - 2.0).abs() < 1e-12);
        step(&mut objs);
        assert!((objs[0].pos[1] - 4.0).abs() < 1e-12);
        step(&mut objs); // осталось 1 ≤ speed → прилипаем к цели
        assert_eq!(objs[0].pos[1], 5.0);
    }

    #[test]
    fn cycles_through_frames_and_wraps() {
        let a = Keyframe { x: Some(1.0), ..Keyframe::default() };
        let b = Keyframe { x: Some(0.0), ..Keyframe::default() };
        let mut objs = vec![animated(vec![a, b])];
        step(&mut objs); // дошли до 1.0 → индекс 1
        assert_eq!(objs[0].pos[0], 1.0);
        assert_eq!(objs[0].anim.as_ref().unwrap().index, 1);
        step(&mut objs); // дошли до 0.0 → индекс снова 0
        assert_eq!(objs[0].pos[0], 0.0);
        assert_eq!(objs[0].anim.as_ref().unwrap().index, 0);
    }

    #[test]
    fn rotation_is_rate_limited() {
        let kf = Keyframe { rr: Some(90.0), ..Keyframe::default() };
        let mut objs = vec![animated(vec![kf])];
        step(&mut objs);
        assert!((objs[0].rot_y - ROT_SPEED).abs() < 1e-12);
        assert_eq!(objs[0].anim.as_ref().unwrap().index, 0, "ещё не приехали");
    }

    #[test]
    fn empty_keyframe_list_is_ignored() {
        let mut objs = vec![animated(vec![])];
        step(&mut objs);
        assert_eq!(objs[0].pos, [0.0; 3]);
    }

    #[test]
    fn objects_without_animation_are_untouched() {
        let mut objs = vec![Object::new(Shape::Sphere)];
        step(&mut objs);
        assert_eq!(objs[0].pos, [0.0; 3]);
    }
}
