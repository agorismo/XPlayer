#!/usr/bin/env python3
"""Adiciona uma versão ao apps.json (fonte do AltStore/SideStore).

uso: update_source.py apps.json VERSÃO BUILD TAMANHO_BYTES URL_DO_IPA "notas" [DATA]
"""
import datetime
import json
import sys


def update(data, version, build, size, url, notes, date=None, min_os="14.0"):
    date = date or datetime.date.today().isoformat()
    app = data["apps"][0]
    entry = {
        "version": version,
        "buildVersion": str(build),
        "date": date,
        "downloadURL": url,
        "localizedDescription": notes,
        "size": int(size),
        "minOSVersion": min_os,
    }
    # a mesma versão+build não entra duas vezes; a mais nova fica primeiro
    versions = [v for v in app.get("versions", []) if (v["version"], v["buildVersion"]) != (version, str(build))]
    app["versions"] = [entry] + versions
    # campos antigos (AltStore 1.x ainda lê)
    app.update(version=version, versionDate=date, versionDescription=notes, downloadURL=url, size=int(size))
    return data


if __name__ == "__main__":
    path, version, build, size, url, notes = sys.argv[1:7]
    date = sys.argv[7] if len(sys.argv) > 7 else None
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    update(data, version, build, size, url, notes, date)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=2)
        f.write("\n")
