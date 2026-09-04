#!/usr/bin/env python3
"""检查 siku-sync-relay 邮箱 SQLite 内容（只读诊断，不修改数据）。

用法（在 relay 服务器上执行）：
    python3 check-mailbox.py [邮箱db路径]
默认路径：/var/lib/siku-relay/relay.json.mailbox.sqlite
"""
import sqlite3
import sys
import time

DB = sys.argv[1] if len(sys.argv) > 1 else "/var/lib/siku-relay/relay.json.mailbox.sqlite"

con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
now = int(time.time())
print(f"db = {DB}")
print(f"now = {now}\n")

total = con.execute("SELECT count(*) FROM mailbox_messages").fetchone()[0]
print(f"total rows: {total}\n")

print("== 按目标设备分组 ==")
for r in con.execute(
    """SELECT to_device_id, count(*), sum(length(ciphertext)),
              min(created_at), max(created_at), min(expires_at), max(expires_at)
       FROM mailbox_messages GROUP BY to_device_id"""
):
    print(
        "to=%r count=%d bytes=%s created=[%s..%s] expires=[%s..%s]" % r
    )

print("\n== 按来源设备分组（账号存档） ==")
for r in con.execute(
    """SELECT from_device_id, count(*), sum(length(ciphertext)),
              min(created_at), max(created_at)
       FROM mailbox_messages WHERE to_device_id='' GROUP BY from_device_id"""
):
    print("from=%s count=%d bytes=%d created=[%s..%s]" % r)

print("\n== 账号存档最新 12 条 ==")
print("%-38s %-38s %12s %12s  %s" % ("id", "from_device", "bytes", "created_at", "seen"))
for r in con.execute(
    """SELECT id, from_device_id, length(ciphertext), created_at, seen
       FROM mailbox_messages WHERE to_device_id=''
       ORDER BY seq DESC LIMIT 12"""
):
    print("%-38s %-38s %12d %12d  %s" % r)

print("\n== per-device 队列全部消息 ==")
for r in con.execute(
    """SELECT id, to_device_id, from_device_id, length(ciphertext), created_at, delivered_at
       FROM mailbox_messages WHERE to_device_id != '' ORDER BY seq DESC LIMIT 20"""
):
    print("id=%s to=%s from=%s bytes=%d created=%s delivered_at=%s" % r)

con.close()
