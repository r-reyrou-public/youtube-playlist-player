"""exe を VirusTotal で検査し、結果の要約を表示する（API キーは環境変数 VT_API_KEY から読む）。

使い方: python tools/vt_check.py <exeのパス>
終了コード: 0=検出なし, 1=検出あり, 2=エラー/未完了
"""
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request
import uuid

API = "https://www.virustotal.com/api/v3"
KEY = os.environ.get("VT_API_KEY")


def req(method, path, data=None, headers=None):
    h = {"x-apikey": KEY, **(headers or {})}
    r = urllib.request.Request(f"{API}{path}", data=data, method=method, headers=h)
    try:
        with urllib.request.urlopen(r, timeout=120) as resp:
            return resp.status, json.load(resp)
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.load(e)
        except Exception:
            return e.code, {}


def upload(path):
    boundary = uuid.uuid4().hex
    with open(path, "rb") as f:
        content = f.read()
    name = os.path.basename(path)
    body = (
        f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{name}"\r\n'
        "Content-Type: application/octet-stream\r\n\r\n"
    ).encode() + content + f"\r\n--{boundary}--\r\n".encode()
    st, js = req("POST", "/files", body, {"Content-Type": f"multipart/form-data; boundary={boundary}"})
    if st != 200:
        sys.exit(f"upload failed: HTTP {st} {js.get('error', '')}")
    return js["data"]["id"]


def summarize(attrs):
    stats = attrs.get("last_analysis_stats") or attrs.get("stats") or {}
    results = attrs.get("last_analysis_results") or attrs.get("results") or {}
    flagged = {k: v.get("result") for k, v in results.items() if v.get("category") in ("malicious", "suspicious")}
    return stats, flagged


def main():
    if not KEY:
        sys.exit("VT_API_KEY が設定されていません")
    path = sys.argv[1]
    sha = hashlib.sha256(open(path, "rb").read()).hexdigest()
    print("sha256:", sha)

    st, js = req("GET", f"/files/{sha}")
    if st == 200 and js["data"]["attributes"].get("last_analysis_stats"):
        print("(VirusTotal に既知のファイル。既存の解析結果を使用)")
        stats, flagged = summarize(js["data"]["attributes"])
    else:
        print("未知のファイルのためアップロードします")
        aid = upload(path)
        for i in range(40):
            time.sleep(15)
            st, js = req("GET", f"/analyses/{aid}")
            if st == 200 and js["data"]["attributes"]["status"] == "completed":
                break
            print(f"  解析待ち... {(i + 1) * 15}s")
        else:
            print("解析が完了しませんでした")
            sys.exit(2)
        stats, flagged = summarize(js["data"]["attributes"])

    print("stats:", stats)
    for engine, res in sorted(flagged.items()):
        print(f"  FLAGGED {engine}: {res}")
    print(f"report: https://www.virustotal.com/gui/file/{sha}")
    sys.exit(1 if (stats.get("malicious", 0) or stats.get("suspicious", 0)) else 0)


if __name__ == "__main__":
    main()
