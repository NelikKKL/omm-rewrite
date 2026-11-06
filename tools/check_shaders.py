#!/usr/bin/env python3
"""Проверка шейдеров и GL-конвейера viewer в headless Chromium (WebGL2).

Берёт GLSL прямо из src/viewer/shaders.rs, рисует тестовые сцены тем же
порядком вызовов, что и viewer/gl.rs, и проверяет пиксели:

  * шейдеры компилируются и линкуются;
  * ориентация текстуры (левый верхний угол картинки = левый верхний угол
    квадрата на экране);
  * плоская закраска грани = цвет × яркость, диагональ квадрата не
    обводится, а границы грани — обводятся;
  * буфер глубины: ближний объект перекрывает дальний при любом порядке;
  * прозрачные текселы пропускают то, что нарисовано позади;
  * проекция/модель в JS-порте совпадают с формулами OMM 3.x.

Запуск:   python3 tools/check_shaders.py
Нужно:    pip install playwright && playwright install chromium
"""
import asyncio
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = (ROOT / "src" / "viewer" / "shaders.rs").read_text(encoding="utf-8")


def grab(name: str) -> str:
    m = re.search(r'pub const ' + name + r': &str = r#"(.*?)"#;', SRC, re.S)
    if not m:
        sys.exit(f"не нашёл {name} в shaders.rs")
    return m.group(1)


PAGE = r"""
<canvas id="c" width="200" height="200"></canvas>
<script>
const FOCAL = 600, NEAR_W = 4;

// ── порт camera::view_proj_f64 ───────────────────────────────────────────────
function viewProj(cam, w, h) {
  const [sRx, cRx] = [Math.sin(cam.rx), Math.cos(cam.rx)];
  const [sRy, cRy] = [Math.sin(cam.ry), Math.cos(cam.ry)];
  const r = [[cRy, sRy*sRx, sRy*cRx], [0, cRx, -sRx], [-sRy, cRy*sRx, cRy*cRx]];
  const d0 = FOCAL + cam.z;
  const far = 2*Math.max(d0, 0) + 1e5;
  const a = (far + NEAR_W)/(far - NEAR_W);
  const b = -2*far*NEAR_W/(far - NEAR_W);
  const p = [
    [2*FOCAL/w, 0, 2*cam.cx/w, 2*cam.cx*d0/w],
    [0, -2*FOCAL/h, -2*cam.cy/h, -2*cam.cy*d0/h],
    [0, 0, a, a*d0 + b],
    [0, 0, 1, d0],
  ];
  const m = new Array(16).fill(0);
  for (let row = 0; row < 4; row++) {
    for (let col = 0; col < 3; col++) {
      let s = 0; for (let k = 0; k < 3; k++) s += p[row][k]*r[k][col];
      m[col*4 + row] = s;
    }
    m[12 + row] = p[row][3];
  }
  return m;
}

// ── порт vm::scene::model_matrix ─────────────────────────────────────────────
function modelMatrix(o) {
  const sx = o.scale, sy = o.scale*(o.squash ?? 1), sz = o.scale;
  const [sRx, cRx] = [Math.sin(o.rotX ?? 0), Math.cos(o.rotX ?? 0)];
  const [sRy, cRy] = [Math.sin(o.rotY ?? 0), Math.cos(o.rotY ?? 0)];
  const c0 = [cRy, 0, -sRy], c1 = [sRy*sRx, cRx, cRy*sRx], c2 = [sRy*cRx, -sRx, cRy*cRx];
  return [
    c0[0]*sx, c0[1]*sx, c0[2]*sx, 0,
    c1[0]*sy, c1[1]*sy, c1[2]*sy, 0,
    c2[0]*sz, c2[1]*sz, c2[2]*sz, 0,
    o.x ?? 0, o.y ?? 0, o.z ?? 0, 1,
  ];
}

// ── порт vm::geometry::tessellate (куб и квадрат) ────────────────────────────
function pushTri(out, verts, idx, light, uv, edge) {
  for (let k = 0; k < 3; k++) out.push(...verts[idx[k]], light, uv[k][0], uv[k][1], edge);
}
function tessellate(verts, faces, lights) {
  const out = [];
  faces.forEach((f, fi) => {
    pushTri(out, verts, [f[0], f[1], f[2]], lights[fi], [[0,0],[1,0],[1,1]], 5);
    pushTri(out, verts, [f[0], f[2], f[3]], lights[fi], [[0,0],[1,1],[0,1]], 3);
  });
  return new Float32Array(out);
}
const CUBE_V = [[-1,-1,-1],[1,-1,-1],[1,1,-1],[-1,1,-1],[-1,-1,1],[1,-1,1],[1,1,1],[-1,1,1]];
const CUBE_F = [[0,1,2,3],[4,5,6,7],[0,1,5,4],[2,3,7,6],[0,3,7,4],[1,2,6,5]];
const CUBE_L = CUBE_F.map((_, i) => Math.min(1, 0.85 + (i % 6)*0.05));
const QUAD_V = [[-1,-1,0],[1,-1,0],[1,1,0],[-1,1,0]];

// ── GL-конвейер: порядок вызовов как в viewer/gl.rs ──────────────────────────
function makeGl(canvas) {
  const gl = canvas.getContext('webgl2', { antialias: true, alpha: true, depth: true, premultipliedAlpha: true });
  if (!gl) throw new Error('нет WebGL2');
  return gl;
}
function compile(gl, vs, fs) {
  const mk = (type, src) => {
    const s = gl.createShader(type); gl.shaderSource(s, src); gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error('шейдер: ' + gl.getShaderInfoLog(s));
    return s;
  };
  const p = gl.createProgram();
  gl.attachShader(p, mk(gl.VERTEX_SHADER, vs)); gl.attachShader(p, mk(gl.FRAGMENT_SHADER, fs));
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error('линковка: ' + gl.getProgramInfoLog(p));
  return p;
}
function gpuMesh(gl, data) {
  const vao = gl.createVertexArray(); gl.bindVertexArray(vao);
  const buf = gl.createBuffer(); gl.bindBuffer(gl.ARRAY_BUFFER, buf);
  gl.bufferData(gl.ARRAY_BUFFER, data, gl.STATIC_DRAW);
  const stride = 7*4;
  gl.enableVertexAttribArray(0); gl.vertexAttribPointer(0, 3, gl.FLOAT, false, stride, 0);
  gl.enableVertexAttribArray(1); gl.vertexAttribPointer(1, 1, gl.FLOAT, false, stride, 12);
  gl.enableVertexAttribArray(2); gl.vertexAttribPointer(2, 2, gl.FLOAT, false, stride, 16);
  gl.enableVertexAttribArray(3); gl.vertexAttribPointer(3, 1, gl.FLOAT, false, stride, 24);
  gl.bindVertexArray(null);
  return { vao, count: data.length/7 };
}
function texFromRgba(gl, w, h, rgba) {
  const t = gl.createTexture(); gl.bindTexture(gl.TEXTURE_2D, t);
  gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
  gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, true);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array(rgba));
  gl.generateMipmap(gl.TEXTURE_2D);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  return t;
}

// scene: { cam, instances: [{mesh, obj, color, tex?}] }, порядок задаёт вызывающий
function render(gl, prog, W, H, cam, opaque, blended, outline) {
  gl.viewport(0, 0, W, H);
  gl.clearColor(0, 0, 0, 0);
  gl.disable(gl.CULL_FACE);
  gl.enable(gl.DEPTH_TEST); gl.depthFunc(gl.LEQUAL); gl.depthMask(true);
  gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
  gl.useProgram(prog);
  const U = n => gl.getUniformLocation(prog, n);
  gl.uniformMatrix4fv(U('u_vp'), false, new Float32Array(viewProj(cam, W, H)));
  gl.uniform1f(U('u_outline'), outline);
  gl.uniform1i(U('u_tex'), 0);
  const draw = inst => {
    gl.uniformMatrix4fv(U('u_model'), false, new Float32Array(modelMatrix(inst.obj)));
    gl.uniform3f(U('u_color'), ...inst.color);
    if (inst.tex) { gl.activeTexture(gl.TEXTURE0); gl.bindTexture(gl.TEXTURE_2D, inst.tex); gl.uniform1f(U('u_use_tex'), 1); }
    else gl.uniform1f(U('u_use_tex'), 0);
    gl.bindVertexArray(inst.mesh.vao);
    gl.drawArrays(gl.TRIANGLES, 0, inst.mesh.count);
  };
  gl.disable(gl.BLEND);
  opaque.forEach(draw);
  gl.enable(gl.BLEND); gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
  blended.forEach(draw);
  gl.disable(gl.BLEND);
}
function px(gl, W, H, x, y) {            // x,y — экранные (y вниз)
  const out = new Uint8Array(4);
  gl.readPixels(x, H - 1 - y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, out);
  return Array.from(out);
}

// ── тесты ────────────────────────────────────────────────────────────────────
window.runAll = (vs, fs) => {
  const res = {};
  const W = 200, H = 200;
  const canvas = document.getElementById('c');
  const gl = makeGl(canvas);
  const prog = compile(gl, vs, fs);
  const cube = gpuMesh(gl, tessellate(CUBE_V, CUBE_F, CUBE_L));
  const quad = gpuMesh(gl, tessellate(QUAD_V, [[0,1,2,3]], [0.85]));
  const cam0 = { z: 600, rx: 0, ry: 0, cx: 0, cy: 0 };
  res.compiled = true;

  // 1. плоский куб: центр грани = цвет × 0.85, край темнее, диагональ не обведена
  render(gl, prog, W, H, cam0, [{ mesh: cube, obj: { scale: 50 }, color: [200/255, 200/255, 200/255] }], [], 0.15);
  res.center = px(gl, W, H, 100, 100);
  res.edge = px(gl, W, H, 74, 100);
  res.outside = px(gl, W, H, 5, 5);
  res.diagA = px(gl, W, H, 90, 90);   // на диагонали квадрата
  res.diagB = px(gl, W, H, 110, 90);  // вне диагонали (для сравнения)

  // 2. без контура центр и край одинаковы
  render(gl, prog, W, H, cam0, [{ mesh: cube, obj: { scale: 50 }, color: [200/255, 200/255, 200/255] }], [], 0);
  res.edgeNoOutline = px(gl, W, H, 74, 100);

  // 3. ориентация текстуры: TL красный, TR зелёный, BL синий, BR жёлтый
  const tex = texFromRgba(gl, 2, 2, [255,0,0,255, 0,255,0,255,   0,0,255,255, 255,255,0,255]);
  render(gl, prog, W, H, cam0, [], [{ mesh: quad, obj: { scale: 50 }, color: [1,1,1], tex }], 0.15);
  res.texTL = px(gl, W, H, 100 - 18, 100 - 18);
  res.texTR = px(gl, W, H, 100 + 18, 100 - 18);
  res.texBL = px(gl, W, H, 100 - 18, 100 + 18);
  res.texBR = px(gl, W, H, 100 + 18, 100 + 18);
  res.texOutside = px(gl, W, H, 20, 20);

  // 4. глубина: ближний красный куб перекрывает дальний синий в любом порядке
  const near = { mesh: cube, obj: { scale: 30, z: -200 }, color: [1, 0, 0] };
  const far = { mesh: cube, obj: { scale: 60, z: 200 }, color: [0, 0, 1] };
  render(gl, prog, W, H, cam0, [near, far], [], 0);
  res.depthNearFirst = px(gl, W, H, 100, 100);
  render(gl, prog, W, H, cam0, [far, near], [], 0);
  res.depthFarFirst = px(gl, W, H, 100, 100);

  // 5. прозрачность: левая половина текстуры прозрачна → виден зелёный куб позади
  const half = texFromRgba(gl, 2, 1, [0,0,0,0, 255,0,0,255]);
  const back = { mesh: cube, obj: { scale: 80, z: 150 }, color: [0, 1, 0] };
  const sprite = { mesh: quad, obj: { scale: 60, z: -100 }, color: [1,1,1], tex: half };
  render(gl, prog, W, H, cam0, [back], [sprite], 0);
  res.alphaLeft = px(gl, W, H, 100 - 25, 100);
  res.alphaRight = px(gl, W, H, 100 + 25, 100);
  // без фона: прозрачный тексел остаётся полностью прозрачным
  render(gl, prog, W, H, cam0, [], [sprite], 0);
  res.alphaEmptyLeft = px(gl, W, H, 100 - 25, 100);

  // 5b. предумноженная фильтрация: посередине между прозрачным и белым
  //     тексселем получаем (≈128, ≈128, ≈128, ≈128), а не тёмную каёмку (≈64)
  const fringe = texFromRgba(gl, 2, 1, [0,0,0,0, 255,255,255,255]);
  render(gl, prog, W, H, cam0, [], [{ mesh: quad, obj: { scale: 60, z: -100 }, color: [1,1,1], tex: fringe }], 0);
  res.fringe = px(gl, W, H, 100, 100);

  // 6. математика: JS-порты против формул OMM 3.x
  const camT = { z: 450, rx: 0.25, ry: -1.1, cx: 12, cy: -7 };
  const m = viewProj(camT, 800, 600);
  const o = { x: 10, y: -20, z: 30, scale: 70, squash: 0.5, rotX: 0.3, rotY: -0.7 };
  const mm = modelMatrix(o);
  let maxErr = 0;
  for (const v of CUBE_V) {
    // OMM 3.x по стадиям
    let p = [v[0]*o.scale, v[1]*o.scale*o.squash, v[2]*o.scale];
    let ty = p[1]*Math.cos(o.rotX) - p[2]*Math.sin(o.rotX), tz = p[1]*Math.sin(o.rotX) + p[2]*Math.cos(o.rotX);
    let tx = p[0]*Math.cos(o.rotY) + tz*Math.sin(o.rotY); tz = -p[0]*Math.sin(o.rotY) + tz*Math.cos(o.rotY);
    const w = [tx + o.x, ty + o.y, tz + o.z];
    let cy = w[1]*Math.cos(camT.rx) - w[2]*Math.sin(camT.rx), cz = w[1]*Math.sin(camT.rx) + w[2]*Math.cos(camT.rx);
    let cx = w[0]*Math.cos(camT.ry) + cz*Math.sin(camT.ry); cz = -w[0]*Math.sin(camT.ry) + cz*Math.cos(camT.ry);
    const sc = FOCAL/(FOCAL + cz + camT.z);
    const want = [cx*sc + 400 + camT.cx, cy*sc + 300 + camT.cy];
    // через наши матрицы
    const M = (mat, q) => [0,1,2,3].map(r => mat[r]*q[0] + mat[4+r]*q[1] + mat[8+r]*q[2] + mat[12+r]);
    const wm = M(mm, v), clip = M(m, wm);
    const got = [(clip[0]/clip[3]*0.5 + 0.5)*800, (0.5 - clip[1]/clip[3]*0.5)*600];
    maxErr = Math.max(maxErr, Math.abs(got[0] - want[0]), Math.abs(got[1] - want[1]));
  }
  res.mathMaxErr = maxErr;
  return res;
};
</script>
"""

CHECKS = []


def check(name, ok, detail=""):
    CHECKS.append((name, bool(ok)))
    print(("  ok   " if ok else "  FAIL ") + name + (f"   {detail}" if detail and not ok else ""))


def close(a, b, tol):
    return all(abs(x - y) <= tol for x, y in zip(a, b))


async def main():
    from playwright.async_api import async_playwright
    vs, fs = grab("VERTEX"), grab("FRAGMENT")
    async with async_playwright() as p:
        browser = await p.chromium.launch(
            args=["--use-angle=swiftshader", "--enable-unsafe-swiftshader",
                  "--ignore-gpu-blocklist", "--use-gl=angle"])
        page = await browser.new_page()
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        await page.set_content(PAGE)
        try:
            r = await page.evaluate("([vs, fs]) => runAll(vs, fs)", [vs, fs])
        except Exception as e:  # ошибки компиляции шейдеров и т.п.
            print("ОШИБКА:", e)
            await browser.close()
            sys.exit(1)
        await browser.close()

    if errors:
        print("JS-ошибки:", errors)
    print(json.dumps({k: v for k, v in r.items() if k != "mathMaxErr"}, ensure_ascii=False))

    check("шейдеры компилируются и линкуются", r["compiled"])
    # 0.85 × 200 = 170
    check("плоская закраска: центр грани = цвет × 0.85 (диагональ не обведена)",
          close(r["center"][:3], [170, 170, 170], 2) and r["center"][3] == 255, r["center"])
    check("диагональ квадрата не темнее остальной грани",
          close(r["diagA"][:3], r["diagB"][:3], 2), (r["diagA"], r["diagB"]))
    check("граница грани обведена (темнее центра)", r["edge"][0] < r["center"][0] - 8, (r["edge"], r["center"]))
    check("без контура край не темнеет", close(r["edgeNoOutline"][:3], r["center"][:3], 2), r["edgeNoOutline"])
    check("вне фигуры полностью прозрачно", r["outside"] == [0, 0, 0, 0], r["outside"])
    check("текстура: верх-лево красный", close(r["texTL"], [255, 0, 0, 255], 3), r["texTL"])
    check("текстура: верх-право зелёный", close(r["texTR"], [0, 255, 0, 255], 3), r["texTR"])
    check("текстура: низ-лево синий", close(r["texBL"], [0, 0, 255, 255], 3), r["texBL"])
    check("текстура: низ-право жёлтый", close(r["texBR"], [255, 255, 0, 255], 3), r["texBR"])
    check("за пределами спрайта прозрачно", r["texOutside"] == [0, 0, 0, 0], r["texOutside"])
    # передняя грань куба светится на 0.85 → красный 255 × 0.85 ≈ 217
    check("глубина: ближний перекрывает дальний (ближний первым)",
          close(r["depthNearFirst"], [217, 0, 0, 255], 2), r["depthNearFirst"])
    check("глубина: ближний перекрывает дальний (дальний первым)",
          close(r["depthFarFirst"], [217, 0, 0, 255], 2), r["depthFarFirst"])
    check("прозрачный тексел пропускает зелёный куб позади",
          close(r["alphaLeft"], [0, 217, 0, 255], 3), r["alphaLeft"])
    check("непрозрачный тексел красный", close(r["alphaRight"], [255, 0, 0, 255], 3), r["alphaRight"])
    check("прозрачный тексел без фона остаётся прозрачным", r["alphaEmptyLeft"] == [0, 0, 0, 0], r["alphaEmptyLeft"])
    check("фильтрация предумноженная (нет тёмной каёмки)",
          close(r["fringe"], [128, 128, 128, 128], 8), r["fringe"])
    check("матрицы воспроизводят проекцию OMM 3.x (< 1e-6 px)", r["mathMaxErr"] < 1e-6, r["mathMaxErr"])

    failed = [n for n, ok in CHECKS if not ok]
    print(f"\n{len(CHECKS) - len(failed)}/{len(CHECKS)} проверок пройдено")
    sys.exit(1 if failed else 0)


asyncio.run(main())
