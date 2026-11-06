//! WebGL2-бэкенд viewer (только для `wasm32`).
//!
//! Принимает [`Scene`] и [`View`] и рисует их. Ни о формате OMM, ни о
//! байт-коде, ни о VM он ничего не знает — только о треугольниках,
//! матрицах и цветах.
//!
//! # Кадр
//!
//! 1. Если `Scene::revision` изменилась — перезагрузить геометрию на GPU.
//! 2. Очистить буферы (фон — полностью прозрачный).
//! 3. Непрозрачные экземпляры — с буфером глубины, в любом порядке.
//! 4. Экземпляры с готовой текстурой — с blending (`ONE, ONE_MINUS_SRC_ALPHA`),
//!    от дальних к ближним.
//!
//! Порядок вызовов проверен на настоящем WebGL2 в headless Chromium
//! (`tools/check_shaders.py`).

use wasm_bindgen::JsCast;
use web_sys::{
    HtmlCanvasElement, HtmlImageElement, WebGl2RenderingContext as Gl, WebGlBuffer, WebGlProgram,
    WebGlShader, WebGlTexture, WebGlUniformLocation, WebGlVertexArrayObject,
};

use super::shaders;
use super::{make_plan, Mesh, Plan, Scene, View, STRIDE};

struct GpuMesh {
    vao: WebGlVertexArrayObject,
    buffer: WebGlBuffer,
    /// Число вершин (кратно 3).
    count: i32,
}

struct Uniforms {
    vp: Option<WebGlUniformLocation>,
    model: Option<WebGlUniformLocation>,
    color: Option<WebGlUniformLocation>,
    tex: Option<WebGlUniformLocation>,
    use_tex: Option<WebGlUniformLocation>,
    outline: Option<WebGlUniformLocation>,
}

pub struct GlViewer {
    gl: Gl,
    program: WebGlProgram,
    u: Uniforms,
    /// Заглушка для сэмплера, пока настоящая текстура не нужна.
    blank: WebGlTexture,
    meshes: Vec<Option<GpuMesh>>,
    textures: Vec<Option<WebGlTexture>>,
    revision: Option<u64>,
    plan: Plan,
}

fn compile(gl: &Gl, kind: u32, source: &str) -> Result<WebGlShader, String> {
    let shader = gl
        .create_shader(kind)
        .ok_or_else(|| "не удалось создать шейдер".to_string())?;
    gl.shader_source(&shader, source);
    gl.compile_shader(&shader);
    if gl
        .get_shader_parameter(&shader, Gl::COMPILE_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        Ok(shader)
    } else {
        Err(format!(
            "ошибка компиляции шейдера: {}",
            gl.get_shader_info_log(&shader).unwrap_or_default()
        ))
    }
}

fn build_program(gl: &Gl) -> Result<WebGlProgram, String> {
    let vs = compile(gl, Gl::VERTEX_SHADER, shaders::VERTEX)?;
    let fs = compile(gl, Gl::FRAGMENT_SHADER, shaders::FRAGMENT)?;
    let program = gl
        .create_program()
        .ok_or_else(|| "не удалось создать программу".to_string())?;
    gl.attach_shader(&program, &vs);
    gl.attach_shader(&program, &fs);
    gl.link_program(&program);
    let linked = gl
        .get_program_parameter(&program, Gl::LINK_STATUS)
        .as_bool()
        .unwrap_or(false);
    gl.delete_shader(Some(&vs));
    gl.delete_shader(Some(&fs));
    if linked {
        Ok(program)
    } else {
        Err(format!(
            "ошибка линковки шейдеров: {}",
            gl.get_program_info_log(&program).unwrap_or_default()
        ))
    }
}

fn make_blank_texture(gl: &Gl) -> Result<WebGlTexture, String> {
    let tex = gl
        .create_texture()
        .ok_or_else(|| "не удалось создать текстуру".to_string())?;
    gl.bind_texture(Gl::TEXTURE_2D, Some(&tex));
    gl.tex_storage_2d(Gl::TEXTURE_2D, 1, Gl::RGBA8, 1, 1);
    Ok(tex)
}

impl GlViewer {
    /// Создаёт WebGL2-контекст на холсте. Холст не должен быть занят
    /// контекстом другого типа (2d, webgl).
    pub fn new(canvas: &HtmlCanvasElement) -> Result<GlViewer, String> {
        let ctx = canvas
            .get_context("webgl2")
            .map_err(|e| format!("getContext('webgl2'): {e:?}"))?
            .ok_or_else(|| "WebGL2 недоступен (или холст уже занят другим контекстом)".to_string())?;
        let gl: Gl = ctx
            .dyn_into::<Gl>()
            .map_err(|_| "полученный контекст не является WebGL2".to_string())?;

        let program = build_program(&gl)?;
        let u = Uniforms {
            vp: gl.get_uniform_location(&program, "u_vp"),
            model: gl.get_uniform_location(&program, "u_model"),
            color: gl.get_uniform_location(&program, "u_color"),
            tex: gl.get_uniform_location(&program, "u_tex"),
            use_tex: gl.get_uniform_location(&program, "u_use_tex"),
            outline: gl.get_uniform_location(&program, "u_outline"),
        };
        let blank = make_blank_texture(&gl)?;

        Ok(GlViewer {
            gl,
            program,
            u,
            blank,
            meshes: Vec::new(),
            textures: Vec::new(),
            revision: None,
            plan: Plan::default(),
        })
    }

    /// Освобождает ресурсы GPU, занятые прежней сценой.
    fn clear_gpu(&mut self) {
        for g in self.meshes.drain(..).flatten() {
            self.gl.delete_vertex_array(Some(&g.vao));
            self.gl.delete_buffer(Some(&g.buffer));
        }
        for t in self.textures.drain(..).flatten() {
            self.gl.delete_texture(Some(&t));
        }
    }

    fn upload_mesh(&self, m: &Mesh) -> Option<GpuMesh> {
        let verts = m.vertex_count() / 3 * 3;
        if verts == 0 {
            return None;
        }
        let floats = m.vertices.get(..verts * STRIDE)?;
        let gl = &self.gl;
        let vao = gl.create_vertex_array()?;
        let buffer = gl.create_buffer()?;

        gl.bind_vertex_array(Some(&vao));
        gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&buffer));
        let mut bytes: Vec<u8> = Vec::with_capacity(floats.len() * 4);
        for f in floats {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        gl.buffer_data_with_u8_array(Gl::ARRAY_BUFFER, &bytes, Gl::STATIC_DRAW);

        // [x y z | light | u v | edge] — см. описание формата в `viewer`
        let stride = (STRIDE * 4) as i32;
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_with_i32(0, 3, Gl::FLOAT, false, stride, 0);
        gl.enable_vertex_attrib_array(1);
        gl.vertex_attrib_pointer_with_i32(1, 1, Gl::FLOAT, false, stride, 12);
        gl.enable_vertex_attrib_array(2);
        gl.vertex_attrib_pointer_with_i32(2, 2, Gl::FLOAT, false, stride, 16);
        gl.enable_vertex_attrib_array(3);
        gl.vertex_attrib_pointer_with_i32(3, 1, Gl::FLOAT, false, stride, 24);
        gl.bind_vertex_array(None);

        Some(GpuMesh { vao, buffer, count: verts as i32 })
    }

    /// Загружает геометрию сцены на GPU и сбрасывает таблицу текстур
    /// (картинки владелец передаёт заново через [`GlViewer::set_texture`]).
    pub fn sync(&mut self, scene: &Scene) {
        self.clear_gpu();
        let uploaded: Vec<Option<GpuMesh>> =
            scene.meshes.iter().map(|m| self.upload_mesh(m)).collect();
        self.meshes = uploaded;
        self.textures = vec![None; scene.textures.len()];
        self.revision = Some(scene.revision);
    }

    /// Загружает картинку как текстуру номер `index` (по `Scene::textures`).
    ///
    /// Для картинок с другого домена WebGL требует CORS: если сервер не
    /// присылает `Access-Control-Allow-Origin`, загрузка завершится ошибкой,
    /// а объект будет нарисован своим цветом.
    pub fn set_texture(&mut self, index: usize, img: &HtmlImageElement) -> Result<(), String> {
        if index >= self.textures.len() {
            return Err(format!("нет текстуры с номером {index}"));
        }
        if !img.complete() || img.natural_width() == 0 || img.natural_height() == 0 {
            return Err("изображение не загружено или повреждено".to_string());
        }
        let gl = &self.gl;
        let tex = gl
            .create_texture()
            .ok_or_else(|| "не удалось создать текстуру".to_string())?;
        gl.active_texture(Gl::TEXTURE0);
        gl.bind_texture(Gl::TEXTURE_2D, Some(&tex));
        gl.pixel_storei(Gl::UNPACK_FLIP_Y_WEBGL, 0);
        // фильтрация предумноженных цветов не даёт тёмных каёмок у прозрачных краёв
        gl.pixel_storei(Gl::UNPACK_PREMULTIPLY_ALPHA_WEBGL, 1);
        if let Err(e) = gl.tex_image_2d_with_u32_and_u32_and_html_image_element(
            Gl::TEXTURE_2D,
            0,
            Gl::RGBA as i32,
            Gl::RGBA,
            Gl::UNSIGNED_BYTE,
            img,
        ) {
            gl.delete_texture(Some(&tex));
            return Err(format!(
                "не удалось загрузить текстуру (для картинок с другого домена нужен CORS): {e:?}"
            ));
        }
        gl.generate_mipmap(Gl::TEXTURE_2D);
        gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_MIN_FILTER, Gl::LINEAR_MIPMAP_LINEAR as i32);
        gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_MAG_FILTER, Gl::LINEAR as i32);
        gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_WRAP_S, Gl::CLAMP_TO_EDGE as i32);
        gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_WRAP_T, Gl::CLAMP_TO_EDGE as i32);

        if let Some(old) = self.textures[index].replace(tex) {
            gl.delete_texture(Some(&old));
        }
        Ok(())
    }

    /// Загружена ли текстура `index`.
    pub fn is_ready(&self, index: usize) -> bool {
        self.textures.get(index).is_some_and(|t| t.is_some())
    }

    fn draw(&self, scene: &Scene, i: u32, textured: bool) {
        let Some(inst) = scene.instances.get(i as usize) else { return };
        let Some(Some(gpu)) = self.meshes.get(inst.mesh as usize) else { return };
        let gl = &self.gl;

        gl.uniform_matrix4fv_with_f32_array(self.u.model.as_ref(), false, &inst.model);
        gl.uniform3f(self.u.color.as_ref(), inst.color[0], inst.color[1], inst.color[2]);
        if textured {
            let tex = inst
                .texture
                .and_then(|t| self.textures.get(t as usize))
                .and_then(|slot| slot.as_ref());
            gl.bind_texture(Gl::TEXTURE_2D, tex);
            gl.uniform1f(self.u.use_tex.as_ref(), 1.0);
        } else {
            gl.uniform1f(self.u.use_tex.as_ref(), 0.0);
        }
        gl.bind_vertex_array(Some(&gpu.vao));
        gl.draw_arrays(Gl::TRIANGLES, 0, gpu.count);
    }

    /// Рисует кадр.
    pub fn render(&mut self, scene: &Scene, view: &View) {
        if self.gl.is_context_lost() {
            return;
        }
        if self.revision != Some(scene.revision) {
            self.sync(scene);
        }

        // план строится без обращения к GL (см. `viewer::make_plan`)
        let mut plan = std::mem::take(&mut self.plan);
        {
            let textures = &self.textures;
            let ready = |t: u32| textures.get(t as usize).is_some_and(|slot| slot.is_some());
            make_plan(scene, view, &ready, &mut plan);
        }

        let gl = &self.gl;
        gl.viewport(0, 0, view.width as i32, view.height as i32);
        gl.clear_color(0.0, 0.0, 0.0, 0.0);
        gl.disable(Gl::CULL_FACE);
        gl.enable(Gl::DEPTH_TEST);
        gl.depth_func(Gl::LEQUAL);
        gl.depth_mask(true);
        gl.clear(Gl::COLOR_BUFFER_BIT | Gl::DEPTH_BUFFER_BIT);

        gl.use_program(Some(&self.program));
        gl.uniform_matrix4fv_with_f32_array(self.u.vp.as_ref(), false, &view.view_proj);
        gl.uniform1f(self.u.outline.as_ref(), view.outline);
        gl.uniform1i(self.u.tex.as_ref(), 0);
        gl.active_texture(Gl::TEXTURE0);
        gl.bind_texture(Gl::TEXTURE_2D, Some(&self.blank));

        gl.disable(Gl::BLEND);
        for &i in &plan.opaque {
            self.draw(scene, i, false);
        }

        gl.enable(Gl::BLEND);
        gl.blend_func(Gl::ONE, Gl::ONE_MINUS_SRC_ALPHA);
        for &i in &plan.blended {
            self.draw(scene, i, true);
        }
        gl.disable(Gl::BLEND);
        gl.bind_vertex_array(None);

        self.plan = plan;
    }
}
