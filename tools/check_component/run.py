#!/usr/bin/env python3
"""Проверка обвязки js/omm-component.js в headless Chromium.

Вместо настоящего WASM-ядра подставляется мок (mock_omm_core.js) с тем же
API: проверяется, что компонент читает модель (текст в теге, .omm/.obj/.glb),
вызывает parse/setup/resize/render, подгружает текстуры, обрабатывает мышь,
колесо, пинч, DPR, ошибки и освобождает движок при удалении элемента.

Запуск:   python3 tools/check_component/run.py
Нужно:    pip install playwright && playwright install chromium
"""
import asyncio, functools, http.server, pathlib, shutil, sys, tempfile, threading

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
SITE = pathlib.Path(tempfile.mkdtemp(prefix="omm-component-"))
shutil.copy(ROOT / "js" / "omm-component.js", SITE / "omm-component.js")
shutil.copy(HERE / "mock_omm_core.js", SITE / "omm_core.js")
for name in ("t.png", "m.omm", "a.obj", "b.glb"):
    shutil.copy(HERE / name, SITE / name)
(SITE / "blank.html").write_text("<!doctype html><meta charset=utf-8>")

class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a): pass

server = http.server.ThreadingHTTPServer(("127.0.0.1", 8765), functools.partial(Quiet, directory=str(SITE)))
threading.Thread(target=server.serve_forever, daemon=True).start()
from playwright.async_api import async_playwright

RESULTS = []
def check(name, ok, detail=""):
    RESULTS.append(bool(ok))
    print(("  ok   " if ok else "  FAIL ") + name + (f"   {detail}" if detail and not ok else ""))

def names(cs): return [c[0] for c in cs]

async def calls(page, i=0):
    return await page.evaluate("(i) => window.__engines[i].calls", i)

async def open_page(browser, scale=1):
    ctx = await browser.new_context(device_scale_factor=scale, viewport={"width": 600, "height": 500})
    page = await ctx.new_page()
    logs = []
    page.on("console", lambda m: logs.append((m.type, m.text)))
    page.on("pageerror", lambda e: logs.append(("pageerror", str(e))))
    await page.route("http://x.test/**", lambda r: r.fulfill(
        path=str(SITE / "t.png"), content_type="image/png", headers={"access-control-allow-origin": "*"}))
    await page.goto("http://127.0.0.1:8765/blank.html")
    await page.evaluate("() => { const s = document.createElement('style'); s.textContent = 'omm-model{width:320px;height:240px}'; document.head.append(s); }")
    return ctx, page, logs

async def mount(page, html, pre=""):
    await page.evaluate("""([html, pre]) => {
        if (pre) (0, eval)(pre);
        document.body.innerHTML = html;
        const s = document.createElement('script'); s.type = 'module'; s.src = '/omm-component.js';
        document.body.append(s);
    }""", [html, pre])

async def wait_calls(page, cond, timeout=8000):
    await page.wait_for_function(f"window.__engines && window.__engines[0] && ({cond})", timeout=timeout)

RENDERS = "window.__engines[0].calls.filter(c => c[0]==='render').length"

async def main():
    async with async_playwright() as p:
        b = await p.chromium.launch(args=["--use-angle=swiftshader", "--enable-unsafe-swiftshader"])

        # 1. текст внутри тега + autorate
        ctx, page, logs = await open_page(b)
        await mount(page, "<omm-model autorate>cube3\ncolor(1,2,3)</omm-model>")
        await wait_calls(page, f"{RENDERS} > 3")
        cs = await calls(page)
        parse = [c for c in cs if c[0] == 'parse']
        check("инлайн-текст передан в parse", len(parse) == 1 and 'cube3' in parse[0][1], parse)
        check("setup вызван", 'setup' in names(cs))
        check("resize по размеру элемента 320×240", ['resize', 320, 240] in cs, [c for c in cs if c[0] == 'resize'])
        check("autorate крутит камеру", 'rot' in names(cs))
        check("анимации нет — step не вызывается", 'step' not in names(cs))
        check("нет ошибок в консоли", not [l for l in logs if l[0] in ('error', 'pageerror')], logs)
        await ctx.close()

        # 2. src: .omm / .obj / .glb
        for src, want in [("m.omm", "cube3\ncolor(1,2,3)\n"), ("a.obj", "obj:8"), ("b.glb", "glb:4")]:
            ctx, page, logs = await open_page(b)
            await mount(page, f'<omm-model src="{src}"></omm-model>')
            await wait_calls(page, "window.__engines[0].calls.some(c => c[0]==='parse')")
            got = [c[1] for c in await calls(page) if c[0] == 'parse'][0]
            check(f"src={src} → {want!r}", got == want, got)
            await ctx.close()

        # 3. freer: ЛКМ вращает, ПКМ двигает, колесо зумит
        ctx, page, logs = await open_page(b)
        await mount(page, "<omm-model freer>cube3</omm-model>")
        await wait_calls(page, f"{RENDERS} > 1")
        box = await page.evaluate("(() => { const r = document.querySelector('omm-model').getBoundingClientRect(); return [r.x, r.y, r.width, r.height]; })()")
        cx, cy = box[0] + box[2] / 2, box[1] + box[3] / 2
        await page.mouse.move(cx, cy); await page.mouse.down(); await page.mouse.move(cx + 30, cy + 10); await page.mouse.up()
        await page.mouse.move(cx, cy); await page.mouse.down(button="right"); await page.mouse.move(cx - 20, cy + 5); await page.mouse.up(button="right")
        await page.mouse.wheel(0, 120)
        await page.wait_for_timeout(200)
        cs = await calls(page)
        drags = [c for c in cs if c[0] == 'drag']
        check("ЛКМ → drag(mode 0)", any(d[3] == 0 and d[1] > 0 for d in drags), drags)
        check("ПКМ → drag(mode 1, панорама)", any(d[3] == 1 and d[1] < 0 for d in drags), drags)
        check("колесо → zoom", any(c[0] == 'zoom' and c[1] == 120 for c in cs), [c for c in cs if c[0] == 'zoom'])
        await ctx.close()

        # 3b. без freer управление отключено
        ctx, page, logs = await open_page(b)
        await mount(page, "<omm-model>cube3</omm-model>")
        await wait_calls(page, f"{RENDERS} > 1")
        box = await page.evaluate("(() => { const r = document.querySelector('omm-model').getBoundingClientRect(); return [r.x, r.y]; })()")
        await page.mouse.move(box[0] + 50, box[1] + 50); await page.mouse.down(); await page.mouse.move(box[0] + 90, box[1] + 60); await page.mouse.up()
        await page.mouse.wheel(0, 100)
        await page.wait_for_timeout(150)
        cs = await calls(page)
        check("без freer мышь не управляет камерой", not any(c[0] in ('drag', 'zoom') for c in cs), cs[-4:])
        await ctx.close()

        # 3c. пинч двумя пальцами (CDP touch)
        ctx, page, logs = await open_page(b)
        await mount(page, "<omm-model freer>cube3</omm-model>")
        await wait_calls(page, f"{RENDERS} > 1")
        box = await page.evaluate("(() => { const r = document.querySelector('omm-model').getBoundingClientRect(); return [r.x, r.y, r.width, r.height]; })()")
        cx, cy = box[0] + box[2] / 2, box[1] + box[3] / 2
        cdp = await ctx.new_cdp_session(page)
        pt = lambda i, x, y: {"x": x, "y": y, "id": i}
        await cdp.send("Input.dispatchTouchEvent", {"type": "touchStart", "touchPoints": [pt(1, cx - 20, cy), pt(2, cx + 20, cy)]})
        await cdp.send("Input.dispatchTouchEvent", {"type": "touchMove", "touchPoints": [pt(1, cx - 40, cy), pt(2, cx + 40, cy)]})
        await cdp.send("Input.dispatchTouchEvent", {"type": "touchEnd", "touchPoints": []})
        await page.wait_for_timeout(150)
        cs = await calls(page)
        pinch = [c for c in cs if c[0] == 'pinch']
        check("пинч → pinch_zoom(z0, d0, d)", pinch and pinch[0][1] == 600 and abs(pinch[0][2] - 40) < 1 and abs(pinch[-1][3] - 80) < 1, pinch)
        await ctx.close()

        # 4. текстуры: pending_textures → Image(crossOrigin) → set_texture
        ctx, page, logs = await open_page(b)
        await mount(page, "<omm-model>image3</omm-model>", "window.__pending = ['http://x.test/pic.png'];")
        await page.wait_for_function("window.__engines && window.__engines[0] && window.__engines[0].textures.length >= 1", timeout=8000)
        tex = await page.evaluate("window.__engines[0].textures")
        check("текстура передана в set_texture с исходным ключом", tex and tex[0][0] == 'http://x.test/pic.png' and tex[0][1] == 4, tex)
        await ctx.close()

        # 5. предупреждения движка уходят в консоль
        ctx, page, logs = await open_page(b)
        await mount(page, "<omm-model>cube3</omm-model>", "window.__warn = ['строка 3: неизвестное свойство `bogus`'];")
        await wait_calls(page, "window.__engines[0].calls.some(c => c[0]==='parse')")
        await page.wait_for_timeout(100)
        check("warnings() → console.warn", any(l[0] == 'warning' and 'bogus' in l[1] for l in logs), logs)
        await ctx.close()

        # 6. ошибка setup → сообщение и событие
        ctx, page, logs = await open_page(b)
        await mount(page, "<omm-model>cube3</omm-model>", "window.__failSetup = true;")
        await page.wait_for_function("document.querySelector('omm-model')?.shadowRoot?.querySelector('.msg')?.classList.contains('on')", timeout=8000)
        msg = await page.evaluate("document.querySelector('omm-model').shadowRoot.querySelector('.msg').textContent")
        check("ошибка показана пользователю", 'WebGL2' in msg, msg)
        await ctx.close()

        # 7. анимация → step; DPR=2 → set_pixel_ratio(2) и холст ×2
        ctx, page, logs = await open_page(b, scale=2)
        await mount(page, "<omm-model>cube3</omm-model>", "window.__anim = true;")
        await wait_calls(page, f"{RENDERS} > 2")
        cs = await calls(page)
        check("has_animation → step каждый кадр", 'step' in names(cs))
        check("DPR=2: set_pixel_ratio(2)", ['ratio', 2] in cs, [c for c in cs if c[0] == 'ratio'])
        check("DPR=2: холст вдвое больше элемента", ['resize', 640, 480] in cs, [c for c in cs if c[0] == 'resize'])
        await ctx.close()

        # 8. remove() освобождает движок и останавливает цикл
        ctx, page, logs = await open_page(b)
        await mount(page, "<omm-model>cube3</omm-model>")
        await wait_calls(page, f"{RENDERS} > 1")
        await page.evaluate("document.querySelector('omm-model').remove()")
        await page.wait_for_timeout(100)
        freed = await page.evaluate("window.__engines[0].freed === true")
        n1 = len(await calls(page)); await page.wait_for_timeout(200); n2 = len(await calls(page))
        check("remove() вызывает free()", freed)
        check("цикл отрисовки остановлен", n1 == n2, (n1, n2))
        await ctx.close()

        # 9. правка текста внутри тега перечитывает модель
        ctx, page, logs = await open_page(b)
        await mount(page, "<omm-model>cube3</omm-model>")
        await wait_calls(page, f"{RENDERS} > 1")
        await page.evaluate("document.querySelector('omm-model').textContent = 'sphere3'")
        await page.wait_for_function("window.__engines[0].calls.filter(c => c[0]==='parse').length >= 2", timeout=4000)
        last = [c[1] for c in await calls(page) if c[0] == 'parse'][-1]
        check("правка текста → повторный parse", last == 'sphere3', last)
        await ctx.close()

        await b.close()
    bad = RESULTS.count(False)
    print(f"\n{len(RESULTS) - bad}/{len(RESULTS)} проверок пройдено")
    sys.exit(1 if bad else 0)

try:
    asyncio.run(main())
finally:
    server.shutdown()
    shutil.rmtree(SITE, ignore_errors=True)
