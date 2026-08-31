#!/usr/bin/env python3
"""第二语言最小解码器（验证物，不进产品）。

只用 python 标准库，只依据 crates/ninja-protocol 的 rustdoc schema
（帧格式 + 信封 + 版本规则）写成——证明「第二个语言只靠文档能写出
解码器」。不 import 任何 Rust 产物。

用法: second_language_decode.py <frames.bin>
  帧格式: u32le(len) || UTF-8 JSON（len 只计 JSON 字节）
  信封:   每条消息必含 v 与 type；本实现支持 v=0
  规则:   v 不符 => 立即退出（不猜）；缺 v/type、坏帧 => 报错退出
消息集: hit / layer / input / spawn / config / theme
输出: 每条消息一行 "type<TAB>v"，供调用方比对。
"""

import json
import struct
import sys

SUPPORTED_V = 0
MAX_FRAME_BYTES = 8 * 1024 * 1024  # 8 MiB


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit("usage: second_language_decode.py <frames.bin>")
    data = open(sys.argv[1], "rb").read()
    i = 0
    while i < len(data):
        if len(data) - i < 4:
            sys.exit("truncated length prefix")
        (n,) = struct.unpack_from("<I", data, i)
        i += 4
        if n == 0 or n > MAX_FRAME_BYTES:
            sys.exit("bad frame len %d" % n)
        if len(data) - i < n:
            sys.exit("truncated payload")
        msg = json.loads(data[i : i + n].decode("utf-8"))
        i += n
        v = msg.get("v")
        if v is None:
            sys.exit("missing v")
        if v != SUPPORTED_V:
            # 协议规则：不支持的 v 必须退出，不能猜。
            print("unsupported v=%d, must exit" % v, file=sys.stderr)
            sys.exit(78)
        t = msg.get("type")
        if t is None:
            sys.exit("missing type")
        print("%s\t%d" % (t, v))


if __name__ == "__main__":
    main()
