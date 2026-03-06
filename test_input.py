#!/usr/bin/env python3
"""End-to-end test: Input commands + Screenshot + Events.

Run inside the compositor (marlow-compositor -c foot), then:
  python3 ~/marlow-compositor/test_input.py

/ Test E2E: comandos de input, screenshot y eventos.
"""

import asyncio
import base64
import sys
sys.path.insert(0, "/home/josemarlow/marlow")

from marlow.platform.linux.compositor_client import MarlowCompositorClient


async def test():
    client = MarlowCompositorClient()
    await client.connect()
    passed = 0
    failed = 0

    # 1. Ping
    assert await client.ping(), "Ping failed"
    print("PASS: ping")
    passed += 1

    # 2. List windows
    windows = await client.list_windows()
    print(f"PASS: list_windows — {len(windows)} windows")
    passed += 1
    for w in windows:
        print(f"  id={w.get('window_id')} title={w.get('title')!r} "
              f"app_id={w.get('app_id')!r} focused={w.get('focused')}")

    # 3. Screenshot (full compositor)
    print("Requesting screenshot...")
    img_b64 = await client.request_screenshot(timeout=3.0)
    if img_b64:
        img_bytes = base64.b64decode(img_b64)
        path = "/tmp/marlow-screenshot.png"
        with open(path, "wb") as f:
            f.write(img_bytes)
        print(f"PASS: screenshot — {len(img_bytes)} bytes saved to {path}")
        passed += 1
    else:
        print("FAIL: screenshot — no data returned")
        failed += 1

    # 4. SendKey (if window exists)
    if windows:
        wid = windows[0]["window_id"]

        # Press and release 'a' (evdev keycode 30)
        ok1 = await client.send_key(wid, 30, True)
        ok2 = await client.send_key(wid, 30, False)
        if ok1 and ok2:
            print("PASS: send_key (a press+release)")
            passed += 1
        else:
            print("FAIL: send_key")
            failed += 1

        # 5. SendText
        result = await client.send_text(wid, "hello")
        if "error" not in result:
            print(f"PASS: send_text — typed {result.get('typed')}/{result.get('total')} chars")
            passed += 1
        else:
            print(f"FAIL: send_text — {result}")
            failed += 1

        # 6. SendClick (center of window)
        w = windows[0]
        cx = w.get("width", 100) / 2
        cy = w.get("height", 100) / 2
        ok = await client.send_click(wid, cx, cy, button=1)
        if ok:
            print(f"PASS: send_click at ({cx}, {cy})")
            passed += 1
        else:
            print("FAIL: send_click")
            failed += 1

        # 7. SendHotkey (Ctrl+C)
        ok = await client.send_hotkey(wid, ["ctrl"], "c")
        if ok:
            print("PASS: send_hotkey (ctrl+c)")
            passed += 1
        else:
            print("FAIL: send_hotkey")
            failed += 1

        # 8. Focus window
        ok = await client.focus_window(wid)
        if ok:
            print("PASS: focus_window")
            passed += 1
        else:
            print("FAIL: focus_window")
            failed += 1

        # 9. GetWindowInfo
        info = await client.get_window_info(wid)
        if info:
            print(f"PASS: get_window_info — title={info.get('title')!r}")
            passed += 1
        else:
            print("FAIL: get_window_info")
            failed += 1
    else:
        print("SKIP: no windows for input tests (launch with -c foot)")

    # 10. Subscribe (basic)
    ok = await client.subscribe(["all"])
    if ok:
        print("PASS: subscribe")
        passed += 1
    else:
        print("FAIL: subscribe")
        failed += 1

    await client.disconnect()
    print(f"\nResults: {passed} passed, {failed} failed")
    if failed > 0:
        sys.exit(1)


asyncio.run(test())
