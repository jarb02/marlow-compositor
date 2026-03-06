#!/usr/bin/env python3
"""End-to-end test: Shadow Mode — Marlow opera ventanas invisibles.

Launch compositor with one foot terminal:
  WAYLAND_DISPLAY=wayland-1 cargo run -- -c foot

Then run this test:
  python3 ~/marlow-compositor/test_shadow_mode.py

/ Test E2E: Shadow Mode — ventanas invisibles para el agente.
"""

import asyncio
import sys
import time

sys.path.insert(0, "/home/josemarlow/marlow")

from marlow.platform.linux.compositor_client import MarlowCompositorClient


async def wait_for_windows(client, count, timeout=10):
    """Poll until we have at least `count` windows in user_space."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        windows = await client.list_windows()
        if len(windows) >= count:
            return windows
        await asyncio.sleep(0.3)
    return await client.list_windows()


async def wait_for_shadow_windows(client, count, timeout=10):
    """Poll until we have at least `count` windows in shadow_space."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        windows = await client.get_shadow_windows()
        if len(windows) >= count:
            return windows
        await asyncio.sleep(0.3)
    return await client.get_shadow_windows()


async def test():
    client = MarlowCompositorClient()
    await client.connect()
    passed = 0
    failed = 0

    # 1. Verify initial state: 1 foot window in user_space
    windows = await wait_for_windows(client, 1, timeout=10)
    if len(windows) >= 1:
        print(f"PASS: {len(windows)} window(s) in user_space")
        passed += 1
    else:
        print(f"FAIL: expected at least 1 window, got {len(windows)}")
        failed += 1
        await client.disconnect()
        print(f"\nResults: {passed} passed, {failed} failed")
        sys.exit(1)

    user_foot_id = windows[0]["window_id"]
    print(f"  User foot: id={user_foot_id}")

    # 2. Verify no shadow windows initially
    shadow = await client.get_shadow_windows()
    if len(shadow) == 0:
        print("PASS: no shadow windows initially")
        passed += 1
    else:
        print(f"FAIL: expected 0 shadow windows, got {len(shadow)}")
        failed += 1

    # 3. LaunchInShadow — spawn foot in shadow_space
    result = await client.launch_in_shadow("foot")
    if "launched" in result:
        print(f"PASS: launch_in_shadow returned: {result}")
        passed += 1
    else:
        print(f"FAIL: launch_in_shadow failed: {result}")
        failed += 1

    # 4. Wait for shadow window to appear
    shadow = await wait_for_shadow_windows(client, 1, timeout=8)
    if len(shadow) >= 1:
        print(f"PASS: {len(shadow)} window(s) in shadow_space")
        passed += 1
    else:
        print(f"FAIL: expected 1 shadow window, got {len(shadow)}")
        failed += 1
        await client.disconnect()
        print(f"\nResults: {passed} passed, {failed} failed")
        sys.exit(1)

    shadow_foot_id = shadow[0]["window_id"]
    print(f"  Shadow foot: id={shadow_foot_id}")

    # 5. Verify user_space still shows only the original window
    user_windows = await client.list_windows()
    user_ids = [w["window_id"] for w in user_windows]
    if shadow_foot_id not in user_ids and user_foot_id in user_ids:
        print("PASS: shadow window NOT visible in list_windows")
        passed += 1
    else:
        print(f"FAIL: shadow window leaked into user_space (user_ids={user_ids})")
        failed += 1

    # 6. Agent focuses shadow window
    ok = await client.focus_window(shadow_foot_id)
    if ok:
        print(f"PASS: agent focused shadow window {shadow_foot_id}")
        passed += 1
    else:
        print(f"FAIL: focus_window({shadow_foot_id}) failed")
        failed += 1

    await asyncio.sleep(0.2)

    # 7. Agent types in shadow window
    result = await client.send_text(shadow_foot_id, "echo shadow mode works")
    typed = result.get("typed", 0)
    if typed == 22:
        print(f"PASS: send_text — typed {typed}/22 chars in shadow window")
        passed += 1
    else:
        print(f"FAIL: send_text — typed {typed}/22 chars (expected 22)")
        failed += 1

    # 8. Send Enter
    ok = await client.send_hotkey(shadow_foot_id, [], "enter")
    if ok:
        print("PASS: send_hotkey (enter) in shadow window")
        passed += 1
    else:
        print("FAIL: send_hotkey (enter)")
        failed += 1

    await asyncio.sleep(0.5)

    # 9. Shadow screenshot — verify content
    img_b64 = await client.request_screenshot(window_id=shadow_foot_id, timeout=3.0)
    if img_b64:
        import base64
        img_bytes = base64.b64decode(img_b64)
        path = "/tmp/marlow-shadow-screenshot.png"
        with open(path, "wb") as f:
            f.write(img_bytes)
        print(f"PASS: shadow screenshot — {len(img_bytes)} bytes saved to {path}")
        passed += 1
    else:
        print("FAIL: shadow screenshot — no data returned")
        failed += 1

    # 10. MoveToUser — promote shadow window to visible
    ok = await client.move_to_user(shadow_foot_id)
    if ok:
        print(f"PASS: move_to_user({shadow_foot_id}) succeeded")
        passed += 1
    else:
        print(f"FAIL: move_to_user({shadow_foot_id})")
        failed += 1

    await asyncio.sleep(0.3)

    # 11. Verify promoted window is now in user_space
    user_windows = await client.list_windows()
    user_ids = [w["window_id"] for w in user_windows]
    if shadow_foot_id in user_ids:
        print(f"PASS: promoted window {shadow_foot_id} now in user_space ({len(user_windows)} windows)")
        passed += 1
    else:
        print(f"FAIL: promoted window not in user_space (ids={user_ids})")
        failed += 1

    # 12. Verify shadow_space is now empty
    shadow = await client.get_shadow_windows()
    if len(shadow) == 0:
        print("PASS: shadow_space empty after promote")
        passed += 1
    else:
        print(f"FAIL: shadow_space still has {len(shadow)} windows")
        failed += 1

    # 13. Full desktop screenshot shows both windows
    img_b64 = await client.request_screenshot(timeout=3.0)
    if img_b64:
        import base64
        img_bytes = base64.b64decode(img_b64)
        path = "/tmp/marlow-shadow-desktop.png"
        with open(path, "wb") as f:
            f.write(img_bytes)
        print(f"PASS: desktop screenshot — {len(img_bytes)} bytes (both windows visible)")
        passed += 1
    else:
        print("FAIL: desktop screenshot failed")
        failed += 1

    # 14. MoveToShadow — demote back to invisible
    ok = await client.move_to_shadow(shadow_foot_id)
    if ok:
        print(f"PASS: move_to_shadow({shadow_foot_id}) succeeded")
        passed += 1
    else:
        print(f"FAIL: move_to_shadow({shadow_foot_id})")
        failed += 1

    await asyncio.sleep(0.3)

    # 15. Verify it's back in shadow
    shadow = await client.get_shadow_windows()
    shadow_ids = [w["window_id"] for w in shadow]
    if shadow_foot_id in shadow_ids:
        print(f"PASS: window {shadow_foot_id} back in shadow_space")
        passed += 1
    else:
        print(f"FAIL: window not in shadow_space (ids={shadow_ids})")
        failed += 1

    # 16. Verify user_space only has original window
    user_windows = await client.list_windows()
    user_ids = [w["window_id"] for w in user_windows]
    if len(user_ids) == 1 and user_foot_id in user_ids:
        print("PASS: user_space back to 1 window after demote")
        passed += 1
    else:
        print(f"FAIL: user_space has {len(user_ids)} windows (ids={user_ids})")
        failed += 1

    await client.disconnect()
    print(f"\nResults: {passed} passed, {failed} failed")
    if failed > 0:
        sys.exit(1)


asyncio.run(test())
