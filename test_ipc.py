#!/usr/bin/env python3
"""Test IPC connection to the Marlow Compositor."""

import asyncio
import sys
sys.path.insert(0, "/home/josemarlow/marlow")

from marlow.platform.linux.compositor_client import MarlowCompositorClient


async def test():
    client = MarlowCompositorClient()
    await client.connect()

    # Ping
    assert await client.ping(), "Ping failed"
    print("PASS: ping")

    # List windows
    windows = await client.list_windows()
    print(f"PASS: list_windows - {len(windows)} windows")
    for w in windows:
        print(f"  - id={w.get('window_id')} title={w.get('title')!r} "
              f"app_id={w.get('app_id')!r} "
              f"geo=({w.get('x')},{w.get('y')},{w.get('width')},{w.get('height')}) "
              f"focused={w.get('focused')}")

    # Focus (if there's a window)
    if windows:
        result = await client.focus_window(windows[0]["window_id"])
        print(f"PASS: focus_window - {result}")

    await client.disconnect()
    print("\nAll tests passed!")


asyncio.run(test())
