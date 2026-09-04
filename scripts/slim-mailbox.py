#!/usr/bin/env python3
"""精简 siku-sync-relay 邮箱存档 + 重置 seen 标记（数据修复，一次性执行）。

背景：
- 存档积压 471MB 重复全量快照；每台设备最新的全量快照（>=5MB 的大消息）
  包含其全部历史，可安全取代它此前投递的所有消息。
- 旧 relay 在 poll 时标记 seen，批量发送失败后消息被错误标记「已读」，
  需清空 seen 让设备重新拉取（客户端按 per-sender 游标去重，重复应用安全）。

执行前必须：systemctl stop siku-sync-relay 并备份 db 文件。

用法：
    python3 slim-mailbox.py [邮箱db路径]          # 先 dry-run 预览
    python3 slim-mailbox.py [邮箱db路径] --apply  # 实际执行
默认路径：/var/lib/siku-relay/relay.json.mailbox.sqlite
"""
import sqlite3
import sys

DB = sys.argv[1] if len(sys.argv) > 1 and not sys.argv[1].startswith("--") \
    else "/var/lib/siku-relay/relay.json.mailbox.sqlite"
APPLY = "--apply" in sys.argv
BIG = 5 * 1024 * 1024  # >=5MB 视为全量快照

con = sqlite3.connect(DB)
cur = con.cursor()

def stats(tag):
    n, b = cur.execute(
        "SELECT count(*), coalesce(sum(length(ciphertext)),0) FROM mailbox_messages WHERE to_device_id=''"
    ).fetchone()
    print(f"{tag}: 存档 {n} 条 / {b/1024/1024:.1f}MB")

stats("清理前")

# 每个来源设备：保留最新一条全量快照及比它新的所有消息，删除更早的
devices = [r[0] for r in cur.execute(
    "SELECT DISTINCT from_device_id FROM mailbox_messages WHERE to_device_id=''")]
to_delete = 0
for dev in devices:
    snap = cur.execute(
        """SELECT max(seq) FROM mailbox_messages
           WHERE to_device_id='' AND from_device_id=? AND length(ciphertext) >= ?""",
        (dev, BIG)).fetchone()[0]
    if snap is None:
        print(f"  {dev[:8]}: 无全量快照，保留其全部消息")
        continue
    n = cur.execute(
        """SELECT count(*) FROM mailbox_messages
           WHERE to_device_id='' AND from_device_id=? AND seq < ?""",
        (dev, snap)).fetchone()[0]
    to_delete += n
    print(f"  {dev[:8]}: 最新快照 seq={snap}，将删除其之前的 {n} 条")
    if APPLY:
        cur.execute(
            """DELETE FROM mailbox_messages
               WHERE to_device_id='' AND from_device_id=? AND seq < ?""",
            (dev, snap))

# 重置 seen：让所有设备重新拉取幸存消息（客户端游标去重）
seen_reset = cur.execute("SELECT count(*) FROM mailbox_messages WHERE seen != ''").fetchone()[0]
print(f"  将重置 {seen_reset} 条消息的 seen 标记")
if APPLY:
    cur.execute("UPDATE mailbox_messages SET seen=''")

if APPLY:
    con.commit()
    stats("清理后")
    print("已提交。")
else:
    con.rollback()
    print("\ndry-run 未做修改。确认无误后加 --apply 执行。")

con.close()
