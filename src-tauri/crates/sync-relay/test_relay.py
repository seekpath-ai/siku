#!/usr/bin/env python3
"""Basic integration test for siku-sync-relay."""
import asyncio
import json
import os
import time

import jwt
import websockets

SECRET = os.environ.get("JWT_SECRET", "siku-dev-secret-change-me")
RELAY_URL = os.environ.get("RELAY_URL", "ws://127.0.0.1:8080/v1/signaling")


def make_token(user_id: str, device_id: str) -> str:
    return jwt.encode(
        {"sub": user_id, "device_id": device_id, "exp": int(time.time()) + 3600},
        SECRET,
        algorithm="HS256",
    )


async def main():
    token_a = make_token("user-1", "device-a")
    token_b = make_token("user-1", "device-b")

    # Client A joins room first.
    async with websockets.connect(f"{RELAY_URL}?token={token_a}") as ws_a:
        await ws_a.send(json.dumps({"type": "join", "payload": {"room_id": "user-1"}}))
        await asyncio.sleep(0.2)

        # Client B joins the same room.
        async with websockets.connect(f"{RELAY_URL}?token={token_b}") as ws_b:
            await ws_b.send(json.dumps({"type": "join", "payload": {"room_id": "user-1"}}))
            await asyncio.sleep(0.2)

            # A should receive its own initial presence (empty room).
            msg_a_init = json.loads(await asyncio.wait_for(ws_a.recv(), timeout=2))
            assert msg_a_init["type"] == "presence", msg_a_init
            print("A received initial presence")

            # B (newcomer) should receive presence with existing peer A.
            msg_b = json.loads(await asyncio.wait_for(ws_b.recv(), timeout=2))
            assert msg_b["type"] == "presence", msg_b
            assert "device-a" in msg_b["payload"]["device_ids"]
            print("B received presence with A")

            # B should also receive peer_online for A (so it knows the host is online).
            msg_b_online = json.loads(await asyncio.wait_for(ws_b.recv(), timeout=2))
            assert msg_b_online["type"] == "peer_online", msg_b_online
            assert msg_b_online["payload"]["device_id"] == "device-a"
            print("B received peer_online for A")

            # A (existing peer) should receive peer_online for B.
            msg_a = json.loads(await asyncio.wait_for(ws_a.recv(), timeout=2))
            assert msg_a["type"] == "peer_online", msg_a
            assert msg_a["payload"]["device_id"] == "device-b"
            print("A received peer_online for B")

            # A sends signal to B.
            await ws_a.send(
                json.dumps(
                    {
                        "type": "signal",
                        "payload": {
                            "to_device_id": "device-b",
                            "data": {"type": "offer", "sdp": "fake-sdp"},
                        },
                    }
                )
            )
            msg_b2 = json.loads(await asyncio.wait_for(ws_b.recv(), timeout=2))
            assert msg_b2["type"] == "signal", msg_b2
            assert msg_b2["payload"]["from_device_id"] == "device-a"
            assert msg_b2["payload"]["data"]["type"] == "Offer"
            print("B received signal from A")

            # A sends relay to B.
            await ws_a.send(
                json.dumps(
                    {
                        "type": "relay",
                        "payload": {
                            "to_device_id": "device-b",
                            "ciphertext": "encrypted-payload",
                            "ttl_seconds": 60,
                        },
                    }
                )
            )
            msg_b3 = json.loads(await asyncio.wait_for(ws_b.recv(), timeout=2))
            assert msg_b3["type"] == "relay", msg_b3
            assert msg_b3["payload"]["from_device_id"] == "device-a"
            assert msg_b3["payload"]["ciphertext"] == "encrypted-payload"
            print("B received relay from A")

    print("All relay tests passed")


if __name__ == "__main__":
    asyncio.run(main())
