// App.swift — @main entry. Configures a unified, dark, transparent-titlebar
// window at runtime (no Info.plist editing from Swift) and hosts RootView.

import SwiftUI
import AppKit
import Darwin

@main
struct OpenSKSApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    init() {
        // The app drives the `opensks-cli` engine over pipes. When launched from
        // Finder/`open`, the app's own stdout/stderr are wired to a LaunchServices
        // pipe whose reader can go away; any write then raises SIGPIPE, whose
        // default action silently terminates the process with NO crash report
        // (which is exactly why a double-clicked launch "did nothing"). A direct
        // terminal run survives because its stdout is a tty. Ignore SIGPIPE so
        // such writes fail with EPIPE instead of killing the GUI app.
        signal(SIGPIPE, SIG_IGN)
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .frame(minWidth: 1040, minHeight: 680)
                .preferredColorScheme(.dark)
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentSize)
        .defaultSize(width: 1280, height: 820)
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.appearance = NSAppearance(named: .darkAqua)
        DispatchQueue.main.async {
            for window in NSApp.windows {
                window.titlebarAppearsTransparent = true
                window.titleVisibility = .hidden
                window.styleMask.insert(.fullSizeContentView)
                window.isMovableByWindowBackground = true
                window.backgroundColor = NSColor(red: 14.0 / 255, green: 16.0 / 255, blue: 21.0 / 255, alpha: 1)
            }
        }
        NSApp.activate(ignoringOtherApps: true)
    }
}
