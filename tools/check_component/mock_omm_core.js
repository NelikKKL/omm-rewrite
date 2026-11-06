// Мок WASM-ядра: только для проверки обвязки компонента.
export default async function init() {}
export class OmmEngine {
  constructor() {
    this.calls = [];
    this.textures = [];
    this.pending = (window.__pending ?? []).slice();
    this.hasAnim = !!window.__anim;
    (window.__engines ??= []).push(this);
  }
  log(name, ...a) { this.calls.push([name, ...a]); }
  setup(canvas) { if (window.__failSetup) throw new Error('WebGL2 недоступен'); this.canvas = canvas; this.log('setup'); }
  resize(w, h) { this.canvas.width = w; this.canvas.height = h; this.log('resize', w, h); }
  set_pixel_ratio(r) { this.log('ratio', r); }
  parse(t) { this.log('parse', t); }
  warnings() { return window.__warn ?? []; }
  pending_textures() { return this.pending; }
  set_texture(src, img) { this.textures.push([src, img.naturalWidth]); this.pending = []; }
  render() { this.log('render'); }
  step() { this.log('step'); }
  auto_rotate() { this.log('rot'); }
  drag(dx, dy, m) { this.log('drag', dx, dy, m); }
  zoom(d) { this.log('zoom', d); }
  pinch_zoom(z, a, b) { this.log('pinch', z, a, b); }
  has_animation() { return this.hasAnim; }
  camera_z() { return 600; }
  free() { this.freed = true; }
}
export const obj_to_omm = (t) => 'obj:' + t.length;
export const glb_to_omm = (b) => 'glb:' + b.length;
