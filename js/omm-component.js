// OMM 4.0 — Web Component <omm-model>.
//
// Тонкая обёртка над WASM-ядром: читает модель (атрибут `src` либо текст
// внутри тега), передаёт её движку, подгружает текстуры, крутит цикл
// отрисовки и превращает мышь/тач в управление камерой.
//
//   <omm-model src="model.omm" autorate></omm-model>    автовращение
//   <omm-model src="model.glb" freer></omm-model>       мышь, тач, зум, панорама
//   <omm-model autorate> cube3 color(80,140,255) </omm-model>
//
// Атрибуты: src, autorate, freer (как в OMM 3.x).
// События:  omm-load (модель разобрана), omm-error (ошибка).
// Свойства: el.engine — экземпляр OmmEngine, el.reload() — перечитать модель.
//
// Картинки-текстуры с другого домена должны отдаваться с CORS
// (заголовок Access-Control-Allow-Origin) — этого требует WebGL.

import init, { OmmEngine, obj_to_omm, glb_to_omm } from './omm_core.js';

const TEMPLATE = document.createElement('template');
TEMPLATE.innerHTML = `
<style>
  :host { display: block; position: relative; width: 300px; height: 300px; overflow: hidden; }
  canvas { display: block; width: 100%; height: 100%; touch-action: none; outline: none; }
  .msg { display: none; position: absolute; left: 8px; right: 8px; bottom: 8px; padding: 4px 8px;
         font: 12px/1.4 system-ui, sans-serif; color: #b00020; background: rgba(255,255,255,.88);
         border-radius: 4px; pointer-events: none; }
  .msg.on { display: block; }
</style>
<canvas part="canvas"></canvas>
<div class="msg" part="message" role="alert"></div>`;

let booted = null;
const boot = () => (booted ??= init());

const MAX_DPR = 3;

class OmmModel extends HTMLElement {
  static get observedAttributes() {
    return ['src', 'autorate', 'freer'];
  }

  constructor() {
    super();
    this.attachShadow({ mode: 'open' }).appendChild(TEMPLATE.content.cloneNode(true));
    this._canvas = this.shadowRoot.querySelector('canvas');
    this._msg = this.shadowRoot.querySelector('.msg');
    this._engine = null;
    this._raf = 0;
    this._visible = true;
    this._token = 0;
    this._dpr = 0;
    this._requested = new Set();
    this._pointers = new Map();
    this._drag = null;
    this._pinch = null;
    this._inline = '';
    this._tick = this._tick.bind(this);
  }

  /** Экземпляр OmmEngine (после инициализации) — для продвинутого использования. */
  get engine() {
    return this._engine;
  }

  /** Перечитать модель (src или текст внутри тега). */
  reload() {
    return this._load();
  }

  async connectedCallback() {
    this._inline = this.textContent;
    this._bindInput();
    this._observe();
    try {
      await boot();
      if (!this.isConnected) return;
      const engine = new OmmEngine();
      engine.setup(this._canvas); // бросает исключение, если нет WebGL2
      this._engine = engine;
    } catch (e) {
      this._fail(e);
      return;
    }
    this._fit();
    await this._load();
    if (this.isConnected && this._engine) this._raf = requestAnimationFrame(this._tick);
  }

  disconnectedCallback() {
    cancelAnimationFrame(this._raf);
    this._raf = 0;
    this._token++;
    for (const o of [this._resizeObs, this._intersectObs, this._mutationObs]) o?.disconnect();
    this._pointers.clear();
    this._drag = this._pinch = null;
    if (this._engine) {
      this._engine.free();
      this._engine = null;
    }
  }

  attributeChangedCallback(name, oldValue, newValue) {
    if (name === 'src' && this._engine && oldValue !== newValue) this._load();
  }

  // ── загрузка модели ──────────────────────────────────────────────────────

  async _load() {
    const token = ++this._token;
    const engine = this._engine;
    if (!engine) return;
    try {
      let text;
      const src = this.getAttribute('src');
      if (src) {
        const url = new URL(src, document.baseURI);
        const res = await fetch(url);
        if (!res.ok) throw new Error(`не удалось загрузить ${src} (HTTP ${res.status})`);
        const ext = url.pathname.split('.').pop().toLowerCase();
        if (ext === 'glb') text = glb_to_omm(new Uint8Array(await res.arrayBuffer()));
        else if (ext === 'obj') text = obj_to_omm(await res.text());
        else text = await res.text();
      } else {
        text = this._inline;
      }
      if (token !== this._token || this._engine !== engine) return; // пришёл более новый запрос
      engine.parse(text);
      this._hideMessage();
      this._reportWarnings(engine);
      this._loadTextures();
      this.dispatchEvent(new CustomEvent('omm-load'));
    } catch (e) {
      if (token === this._token) this._fail(e);
    }
  }

  _reportWarnings(engine) {
    const warnings = engine.warnings?.() ?? [];
    for (const w of warnings) console.warn('[omm]', w);
  }

  _loadTextures() {
    const engine = this._engine;
    for (const src of engine.pending_textures()) {
      if (this._requested.has(src)) continue;
      this._requested.add(src);
      const img = new Image();
      // WebGL требует CORS-чистых картинок; data: и тот же origin этого не касаются
      if (!src.startsWith('data:')) img.crossOrigin = 'anonymous';
      img.decoding = 'async';
      img.onload = () => {
        if (this._engine === engine) {
          engine.set_texture(src, img);
          this._reportWarnings(engine);
        }
      };
      img.onerror = () => console.warn('[omm] не удалось загрузить текстуру:', src.slice(0, 120));
      img.src = src;
    }
  }

  // ── размер и видимость ───────────────────────────────────────────────────

  _fit() {
    const engine = this._engine;
    if (!engine) return;
    const dpr = Math.min(window.devicePixelRatio || 1, MAX_DPR);
    const w = Math.max(1, Math.round(this.clientWidth * dpr));
    const h = Math.max(1, Math.round(this.clientHeight * dpr));
    if (this._canvas.width === w && this._canvas.height === h && this._dpr === dpr) return;
    this._dpr = dpr;
    engine.set_pixel_ratio?.(dpr);
    engine.resize(w, h);
  }

  _observe() {
    this._resizeObs = new ResizeObserver(() => this._fit());
    this._resizeObs.observe(this);
    this._intersectObs = new IntersectionObserver((entries) => {
      for (const e of entries) this._visible = e.isIntersecting;
    });
    this._intersectObs.observe(this);
    // текст внутри тега можно править на лету
    this._mutationObs = new MutationObserver(() => {
      if (this.hasAttribute('src')) return;
      this._inline = this.textContent;
      this._load();
    });
    this._mutationObs.observe(this, { childList: true, characterData: true, subtree: true });
  }

  // ── цикл отрисовки ───────────────────────────────────────────────────────

  _tick() {
    this._raf = requestAnimationFrame(this._tick);
    const engine = this._engine;
    if (!engine || !this._visible || document.hidden) return;
    if (engine.has_animation()) engine.step();
    if (this.hasAttribute('autorate') && !this._drag && !this._pinch) engine.auto_rotate();
    engine.render();
  }

  // ── управление (freer) ───────────────────────────────────────────────────

  _bindInput() {
    const c = this._canvas;
    c.addEventListener('pointerdown', (e) => this._down(e));
    c.addEventListener('pointermove', (e) => this._move(e));
    c.addEventListener('pointerup', (e) => this._up(e));
    c.addEventListener('pointercancel', (e) => this._up(e));
    c.addEventListener('wheel', (e) => this._wheel(e), { passive: false });
    c.addEventListener('contextmenu', (e) => {
      if (this.hasAttribute('freer')) e.preventDefault();
    });
  }

  _distance() {
    const [a, b] = [...this._pointers.values()];
    return a && b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
  }

  _down(e) {
    if (!this._engine || !this.hasAttribute('freer')) return;
    this._canvas.setPointerCapture?.(e.pointerId);
    this._pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (this._pointers.size === 2) {
      this._drag = null;
      this._pinch = { z: this._engine.camera_z(), d: this._distance() };
    } else if (this._pointers.size === 1) {
      const pan = e.button === 1 || e.button === 2; // средняя / правая кнопка
      this._drag = { mode: pan ? 1 : 0 };
    }
    e.preventDefault();
  }

  _move(e) {
    const p = this._pointers.get(e.pointerId);
    if (!p || !this._engine) return;
    const dx = e.clientX - p.x;
    const dy = e.clientY - p.y;
    p.x = e.clientX;
    p.y = e.clientY;
    if (this._pinch && this._pointers.size >= 2) {
      const d = this._distance();
      if (d > 0) this._engine.pinch_zoom(this._pinch.z, this._pinch.d, d);
    } else if (this._drag) {
      this._engine.drag(dx, dy, this._drag.mode);
    }
  }

  _up(e) {
    this._pointers.delete(e.pointerId);
    if (this._pointers.size < 2) this._pinch = null;
    if (this._pointers.size === 0) this._drag = null;
    else if (this._pointers.size === 1) this._drag = { mode: 0 }; // остался один палец
  }

  _wheel(e) {
    if (!this._engine || !this.hasAttribute('freer')) return;
    e.preventDefault();
    const k = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 100 : 1;
    this._engine.zoom(e.deltaY * k);
  }

  // ── сообщения ────────────────────────────────────────────────────────────

  _fail(err) {
    const text = String(err?.message ?? err);
    console.error('[omm]', err);
    this._msg.textContent = text;
    this._msg.classList.add('on');
    this.dispatchEvent(new CustomEvent('omm-error', { detail: err }));
  }

  _hideMessage() {
    this._msg.classList.remove('on');
  }
}

if (!customElements.get('omm-model')) customElements.define('omm-model', OmmModel);

export { OmmModel };
