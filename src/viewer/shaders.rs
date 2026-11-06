//! Исходники шейдеров (GLSL ES 3.00 / WebGL2).
//!
//! Лежат в обычном модуле без зависимостей, чтобы их можно было проверить
//! в любом окружении (в том числе прогнать в headless-браузере).
//!
//! Вершинный шейдер получает «плоские» треугольники (по 3 вершины, без
//! индексов): барицентрические координаты вычисляются из `gl_VertexID % 3`,
//! по ним фрагментный шейдер рисует тонкий контур на выбранных рёбрах
//! (`edge` — битовая маска, см. описание формата вершины в `viewer`).

/// Вершинный шейдер.
pub const VERTEX: &str = r#"#version 300 es
precision highp float;

layout(location = 0) in vec3 a_pos;
layout(location = 1) in float a_light;
layout(location = 2) in vec2 a_uv;
layout(location = 3) in float a_edge;

uniform mat4 u_vp;
uniform mat4 u_model;

out float v_light;
out vec2 v_uv;
out vec3 v_bary;
flat out float v_edge;

void main() {
    int corner = gl_VertexID % 3;
    v_bary = vec3(corner == 0 ? 1.0 : 0.0, corner == 1 ? 1.0 : 0.0, corner == 2 ? 1.0 : 0.0);
    v_light = a_light;
    v_uv = a_uv;
    v_edge = a_edge;
    gl_Position = u_vp * (u_model * vec4(a_pos, 1.0));
}
"#;

/// Фрагментный шейдер.
///
/// * `u_use_tex > 0.5` — рисуется текстура (без освещения, как и раньше).
///   Картинки загружаются с предумноженной альфой
///   (`UNPACK_PREMULTIPLY_ALPHA_WEBGL`): фильтрация тогда не даёт тёмных
///   каёмок на границах прозрачности, а результат сразу подходит для
///   canvas с `premultipliedAlpha` и blending `ONE, ONE_MINUS_SRC_ALPHA`.
/// * иначе — цвет экземпляра × яркость вершины, плюс контур толщиной
///   около 1 пикселя с прозрачностью `u_outline`.
pub const FRAGMENT: &str = r#"#version 300 es
precision highp float;

in float v_light;
in vec2 v_uv;
in vec3 v_bary;
flat in float v_edge;

uniform vec3 u_color;
uniform sampler2D u_tex;
uniform float u_use_tex;
uniform float u_outline;

out vec4 o_color;

void main() {
    if (u_use_tex > 0.5) {
        vec4 t = texture(u_tex, v_uv);
        if (t.a < 0.004) {
            discard;
        }
        o_color = t;
        return;
    }

    vec3 col = u_color * v_light;
    if (u_outline > 0.0) {
        int bits = int(v_edge + 0.5);
        vec3 d = v_bary / max(fwidth(v_bary), vec3(1e-6));
        float e = 1e6;
        if ((bits & 1) != 0) { e = min(e, d.x); }
        if ((bits & 2) != 0) { e = min(e, d.y); }
        if ((bits & 4) != 0) { e = min(e, d.z); }
        float line = 1.0 - smoothstep(0.5, 1.5, e);
        col = mix(col, vec3(0.0), u_outline * line);
    }
    o_color = vec4(col, 1.0);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_directive_is_the_very_first_thing() {
        assert!(VERTEX.starts_with("#version 300 es\n"));
        assert!(FRAGMENT.starts_with("#version 300 es\n"));
    }

    #[test]
    fn interface_names_used_by_the_backend_are_present() {
        for name in ["a_pos", "a_light", "a_uv", "a_edge", "u_vp", "u_model"] {
            assert!(VERTEX.contains(name), "{name}");
        }
        for name in ["u_color", "u_tex", "u_use_tex", "u_outline"] {
            assert!(FRAGMENT.contains(name), "{name}");
        }
    }
}
