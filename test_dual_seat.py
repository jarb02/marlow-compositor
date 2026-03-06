#!/usr/bin/env python3
"""End-to-end test: Dual-seat — user and agent operate simultaneously.

Launch compositor with two foot terminals:
  WAYLAND_DISPLAY=wayland-1 cargo run -- -c foot -c foot

Then run this test:
  python3 ~/marlow-compositor/test_dual_seat.py

/ Test E2E: Dual-seat — usuario y agente operan simultaneamente.
"""

import asyncio
import os
import subprocess
import sys
import time

sys.path.insert(0, "/home/josemarlow/marlow")

from marlow.platform.linux.compositor_client import MarlowCompositorClient


async def wait_for_windows(client, count, timeout=10):
    """Poll until we have at least `count` windows."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        windows = await client.list_windows()
        if len(windows) >= count:
            return windows
        await asyncio.sleep(0.3)
    return await client.list_windows()


async def test():
    client = MarlowCompositorClient()
    await client.connect()
    passed = 0
    failed = 0

    # 1. Wait for 2 windows (launched with -c foot -c foot)
    windows = await wait_for_windows(client, 2, timeout=10)
    if len(windows) >= 2:
        print(f"PASS: {len(windows)} windows detected")
        passed += 1
    else:
        # If only 1 window, try spawning another foot via the compositor's display
        status = await client.get_seat_status()
        display = status.get("wayland_display", "")
        if display:
            env = {**os.environ, "WAYLAND_DISPLAY": display}
            subprocess.Popen(["foot"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            print(f"Spawned second foot on {display}, waiting...")
            windows = await wait_for_windows(client, 2, timeout=8)

        if len(windows) >= 2:
            print(f"PASS: {len(windows)} windows detected (after spawn)")
            passed += 1
        else:
            print(f"FAIL: only {len(windows)} windows, need 2")
            failed += 1
            await client.disconnect()
            print(f"\nResults: {passed} passed, {failed} failed")
            sys.exit(1)

    for w in windows:
        print(f"  id={w.get('window_id')} title={w.get('title')!r} "
              f"app_id={w.get('app_id')!r} focused={w.get('focused')}")

    wid_0 = windows[0]["window_id"]
    wid_1 = windows[1]["window_id"]

    # 2. GetSeatStatus — initial state
    status = await client.get_seat_status()
    print(f"Initial seat status: user_focus={status.get('user_focus')} "
          f"agent_focus={status.get('agent_focus')} conflict={status.get('conflict')}")
    if "user_focus" in status and "agent_focus" in status:
        print("PASS: get_seat_status works")
        passed += 1
    else:
        print("FAIL: get_seat_status missing fields")
        failed += 1

    # 3. Agent focuses window 1 (the second terminal)
    ok = await client.focus_window(wid_1)
    if ok:
        print(f"PASS: agent focused window {wid_1}")
        passed += 1
    else:
        print(f"FAIL: agent focus_window({wid_1})")
        failed += 1

    await asyncio.sleep(0.1)

    # 4. Verify seat status: agent_focus should be window 1
    status = await client.get_seat_status()
    agent_focus = status.get("agent_focus")
    user_focus = status.get("user_focus")
    conflict = status.get("conflict", False)
    print(f"After agent focus: user_focus={user_focus} agent_focus={agent_focus} conflict={conflict}")

    if agent_focus == wid_1:
        print("PASS: agent_focus is correct")
        passed += 1
    else:
        print(f"FAIL: expected agent_focus={wid_1}, got {agent_focus}")
        failed += 1

    # 5. Verify no conflict (user and agent on different windows)
    if not conflict:
        print("PASS: no conflict (independent focus)")
        passed += 1
    else:
        print("FAIL: unexpected conflict detected")
        failed += 1

    # 6. Agent types text in window 1
    result = await client.send_text(wid_1, "echo marlow was here")
    typed = result.get("typed", 0)
    if typed == 20:
        print(f"PASS: send_text — typed {typed}/20 chars in window {wid_1}")
        passed += 1
    else:
        print(f"FAIL: send_text — typed {typed}/20 chars (expected 20)")
        failed += 1

    # 7. Send Enter to execute the command
    ok = await client.send_hotkey(wid_1, [], "enter")
    if ok:
        print("PASS: send_hotkey (enter) in window 1")
        passed += 1
    else:
        print("FAIL: send_hotkey (enter)")
        failed += 1

    await asyncio.sleep(0.3)

    # 8. Verify seat status still shows independent focus
    status = await client.get_seat_status()
    agent_focus = status.get("agent_focus")
    print(f"After typing: agent_focus={agent_focus}")
    if agent_focus == wid_1:
        print("PASS: agent still focused on window 1 after typing")
        passed += 1
    else:
        print(f"FAIL: agent focus changed unexpectedly to {agent_focus}")
        failed += 1

    # 9. Screenshot — verify both terminals have content
    img_b64 = await client.request_screenshot(timeout=3.0)
    if img_b64:
        import base64
        img_bytes = base64.b64decode(img_b64)
        path = "/tmp/marlow-dual-seat.png"
        with open(path, "wb") as f:
            f.write(img_bytes)
        print(f"PASS: screenshot — {len(img_bytes)} bytes saved to {path}")
        passed += 1
    else:
        print("FAIL: screenshot — no data returned")
        failed += 1

    # 10. Get window info for both windows — verify they exist independently
    info_0 = await client.get_window_info(wid_0)
    info_1 = await client.get_window_info(wid_1)
    if info_0 and info_1:
        print(f"PASS: both windows queryable — "
              f"win0='{info_0.get('title')}' win1='{info_1.get('title')}'")
        passed += 1
    else:
        print("FAIL: could not query both windows")
        failed += 1

    await client.disconnect()
    print(f"\nResults: {passed} passed, {failed} failed")
    if failed > 0:
        sys.exit(1)


asyncio.run(test())
