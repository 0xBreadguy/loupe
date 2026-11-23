import QtQuick
import Quickshell.Io
import qs.Commons as Commons
import "../Status.js" as Status

// The dashboard's provider hues, from the current Omarchy theme's colors.toml:
// Claude magenta, OpenAI green, Grok blue, Gemini a blue apart from those,
// anything else muted, as the dashboard picks them. Omarchy's Color singleton
// doesn't expose those names, so the theme file is read here.
QtObject {
  id: root
  property var named: ({})

  function hue(family) {
    if (family === "gemini") return Status.geminiHue(named)
    var key = family === "claude" ? "magenta" : family === "openai" ? "green" : family === "grok" ? "blue" : ""
    return key && named[key] ? named[key] : Commons.Color.muted
  }

  function load(text) {
    var out = {}
    var lines = String(text || "").split("\n")
    for (var i = 0; i < lines.length; i++) {
      var m = lines[i].match(/^\s*([a-z_]+)\s*=\s*["'](#[0-9a-fA-F]{6})["']/)
      if (m) out[m[1]] = m[2]
    }
    named = out
  }

  function reload() { file.reload() }

  property FileView file: FileView {
    path: Commons.Color.currentThemePath + "/colors.toml"
    watchChanges: true
    printErrors: false
    onLoaded: root.load(text())
    onLoadFailed: root.load("")
    onFileChanged: reload()
  }

  // A theme switch replaces the folder behind the current-theme link, which a
  // file watch doesn't always see, and the shell pushes the new colors itself.
  // Read the file again whenever the theme's own colors change.
  property Connections themeSwitch: Connections {
    target: Commons.Color
    function onForegroundChanged() { root.reload() }
    function onBackgroundChanged() { root.reload() }
    function onAccentChanged() { root.reload() }
  }
}
