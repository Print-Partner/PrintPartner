// Accessibility probe for the macOS packaged smoke (desktop/packaged-smoke/macos_ui.py).
// Build: swiftc -O ax_probe.swift -o pp-ax
// Commands print one JSON value on stdout:
//   trusted                     whether this process may use the Accessibility API
//   dump LIMIT PID...           the AX tree of each process's windows, at most LIMIT nodes
//   press PID PATH              AXPress the element at PATH (window index, then child indices)
//   type PID PATH TEXT          focus the element at PATH and type TEXT as key events
//   tray PID ITEM               click the app's menu bar extra and press the menu item ITEM
//   cgwindows PID...            on-screen window numbers and bounds for screencapture -l
//   colors PNG [WIDTH X Y W H]  the number of distinct opaque pixel colors in PNG, or only in the
//                               rectangle X Y W H (points) of a window capture WIDTH points wide
import AppKit
import ApplicationServices
import Foundation

func emit(_ value: Any) {
    let data = try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write("\n".data(using: .utf8)!)
}

func fail(_ message: String) -> Never {
    FileHandle.standardError.write((message + "\n").data(using: .utf8)!)
    exit(2)
}

func copy(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var value: CFTypeRef?
    return AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success ? value : nil
}

func text(_ element: AXUIElement, _ name: String) -> String? {
    guard let value = copy(element, name) as? String, !value.isEmpty else { return nil }
    return value
}

func elements(_ element: AXUIElement, _ name: String) -> [AXUIElement] {
    guard let value = copy(element, name), CFGetTypeID(value) == CFArrayGetTypeID() else { return [] }
    return (value as? [AXUIElement]) ?? []
}

func geometry(_ element: AXUIElement, _ name: String, _ type: AXValueType) -> CGRect? {
    guard let value = copy(element, name), CFGetTypeID(value) == AXValueGetTypeID() else { return nil }
    if type == .cgPoint {
        var point = CGPoint.zero
        return AXValueGetValue(value as! AXValue, .cgPoint, &point) ? CGRect(origin: point, size: .zero) : nil
    }
    var size = CGSize.zero
    return AXValueGetValue(value as! AXValue, .cgSize, &size) ? CGRect(origin: .zero, size: size) : nil
}

func application(_ pid: pid_t) -> AXUIElement {
    let app = AXUIElementCreateApplication(pid)
    AXUIElementSetMessagingTimeout(app, 3)
    return app
}

var budget = 0

func node(_ element: AXUIElement, _ depth: Int) -> [String: Any] {
    budget -= 1
    var result: [String: Any] = [:]
    let names = [("r", kAXRoleAttribute), ("s", kAXSubroleAttribute), ("t", kAXTitleAttribute),
                 ("d", kAXDescriptionAttribute), ("v", kAXValueAttribute), ("p", "AXPlaceholderValue")]
    for (key, name) in names {
        if let value = text(element, name) { result[key] = value }
    }
    if let label = copy(element, kAXTitleUIElementAttribute), CFGetTypeID(label) == AXUIElementGetTypeID() {
        let title = label as! AXUIElement
        if let value = text(title, kAXValueAttribute) ?? text(title, kAXTitleAttribute) { result["l"] = value }
    }
    if let size = geometry(element, kAXSizeAttribute, .cgSize)?.size {
        result["w"] = Int(size.width)
        result["h"] = Int(size.height)
    }
    if let origin = geometry(element, kAXPositionAttribute, .cgPoint)?.origin {
        result["x"] = Int(origin.x)
        result["y"] = Int(origin.y)
    }
    if let minimized = copy(element, kAXMinimizedAttribute) as? Bool { result["min"] = minimized }
    var children: [[String: Any]] = []
    if depth < 256 {
        for child in elements(element, kAXChildrenAttribute) {
            if budget <= 0 { break }
            children.append(node(child, depth + 1))
        }
    }
    if !children.isEmpty { result["c"] = children }
    return result
}

func find(_ element: AXUIElement, role: String, title: String, depth: Int = 0) -> AXUIElement? {
    if text(element, kAXRoleAttribute) == role && text(element, kAXTitleAttribute) == title { return element }
    guard depth < 12 else { return nil }
    for child in elements(element, kAXChildrenAttribute) {
        if let found = find(child, role: role, title: title, depth: depth + 1) { return found }
    }
    return nil
}

func resolve(_ pid: pid_t, _ path: String) -> AXUIElement {
    let indices = path.split(separator: ".").compactMap { Int($0) }
    guard let first = indices.first else { fail("empty path") }
    let windows = elements(application(pid), kAXWindowsAttribute)
    guard first < windows.count else { fail("window \(first) is gone") }
    var element = windows[first]
    for index in indices.dropFirst() {
        let children = elements(element, kAXChildrenAttribute)
        guard index < children.count else { fail("path \(path) is gone") }
        element = children[index]
    }
    return element
}

let args = CommandLine.arguments
guard args.count >= 2 else { fail("usage: pp-ax trusted|dump|press|type|cgwindows|colors ...") }
switch args[1] {
case "trusted":
    emit(["trusted": AXIsProcessTrusted()])
case "dump":
    guard args.count >= 3, let limit = Int(args[2]) else { fail("dump LIMIT PID...") }
    budget = limit
    var apps: [[String: Any]] = []
    for pid in args.dropFirst(3).compactMap({ pid_t($0) }) {
        let windows = elements(application(pid), kAXWindowsAttribute).map { node($0, 0) }
        apps.append(["pid": Int(pid), "windows": windows])
    }
    emit(["apps": apps, "truncated": budget <= 0])
case "press":
    guard args.count == 4, let pid = pid_t(args[2]) else { fail("press PID PATH") }
    let status = AXUIElementPerformAction(resolve(pid, args[3]), kAXPressAction as CFString)
    emit(["pressed": status == .success, "status": Int(status.rawValue)])
case "type":
    guard args.count == 5, let pid = pid_t(args[2]) else { fail("type PID PATH TEXT") }
    let element = resolve(pid, args[3])
    NSRunningApplication(processIdentifier: pid)?.activate()
    let focused = AXUIElementSetAttributeValue(element, kAXFocusedAttribute as CFString, kCFBooleanTrue)
    usleep(300_000)
    let source = CGEventSource(stateID: .hidSystemState)
    for unit in args[4].utf16 {
        var character = unit
        for down in [true, false] {
            guard let event = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: down) else {
                fail("cannot create a key event")
            }
            event.keyboardSetUnicodeString(stringLength: 1, unicodeString: &character)
            event.postToPid(pid)
            usleep(15_000)
        }
    }
    emit(["focused": focused == .success, "typed": args[4].utf16.count])
case "tray":
    guard args.count == 4, let pid = pid_t(args[2]) else { fail("tray PID ITEM") }
    let app = application(pid)
    guard let value = copy(app, "AXExtrasMenuBar"),
          CFGetTypeID(value) == AXUIElementGetTypeID(),
          let extra = elements(value as! AXUIElement, kAXChildrenAttribute).first else {
        emit(["pressed": false, "reason": "no menu bar extra"])
        exit(0)
    }
    AXUIElementSetMessagingTimeout(extra, 3)
    var report: [String: Any] = [:]
    // tray-icon attaches the menu only in its own mouseDown handler, so an
    // AXPress on the status button opens nothing: click it for real.
    guard let origin = geometry(extra, kAXPositionAttribute, .cgPoint)?.origin,
          let size = geometry(extra, kAXSizeAttribute, .cgSize)?.size else {
        emit(["pressed": false, "reason": "no menu bar extra frame"])
        exit(0)
    }
    let center = CGPoint(x: origin.x + size.width / 2, y: origin.y + size.height / 2)
    let source = CGEventSource(stateID: .hidSystemState)
    for type in [CGEventType.leftMouseDown, .leftMouseUp] {
        CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: center, mouseButton: .left)?
            .post(tap: .cghidEventTap)
        usleep(50_000)
    }
    report["clicked"] = [Double(center.x), Double(center.y)]
    var item: AXUIElement?
    for _ in 0..<20 where item == nil {
        usleep(250_000)
        // The app's own menu bar also has a Quit item; only the extra's
        // menu, or a menu outside the menu bar, is the tray's.
        item = find(extra, role: kAXMenuItemRole, title: args[3])
        for child in elements(app, kAXChildrenAttribute)
        where item == nil && text(child, kAXRoleAttribute) != kAXMenuBarRole {
            item = find(child, role: kAXMenuItemRole, title: args[3])
        }
    }
    guard let menuItem = item else {
        var seen: [String] = []
        func collect(_ element: AXUIElement, _ depth: Int) {
            guard seen.count < 80, depth < 6 else { return }
            seen.append(String(repeating: " ", count: depth)
                + "\(text(element, kAXRoleAttribute) ?? "?") \(text(element, kAXTitleAttribute) ?? "")")
            for child in elements(element, kAXChildrenAttribute) { collect(child, depth + 1) }
        }
        collect(extra, 0)
        collect(app, 0)
        report["pressed"] = false
        report["reason"] = "no menu item \(args[3])"
        report["seen"] = seen
        emit(report)
        exit(0)
    }
    let status = AXUIElementPerformAction(menuItem, kAXPressAction as CFString)
    report["pressed"] = status == .success
    emit(report)
case "cgwindows":
    let pids = Set(args.dropFirst(2).compactMap { Int($0) })
    let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []
    var found: [[String: Any]] = []
    for info in list {
        guard let owner = info[kCGWindowOwnerPID as String] as? Int, pids.contains(owner),
              let bounds = info[kCGWindowBounds as String] as? NSDictionary,
              let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { continue }
        found.append([
            "id": info[kCGWindowNumber as String] as? Int ?? 0, "pid": owner,
            "layer": info[kCGWindowLayer as String] as? Int ?? 0,
            "onscreen": info[kCGWindowIsOnscreen as String] as? Bool ?? false,
            "x": Int(rect.origin.x), "y": Int(rect.origin.y),
            "w": Int(rect.size.width), "h": Int(rect.size.height),
        ])
    }
    emit(found)
case "colors":
    guard args.count == 3 || args.count == 8 else { fail("colors PNG [WIDTH X Y W H]") }
    guard let image = NSImage(contentsOfFile: args[2]),
          let whole = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
        emit(["colors": 0])
        exit(0)
    }
    var picture = whole
    if args.count == 8 {
        let numbers = args[3...7].compactMap { Double($0) }
        guard numbers.count == 5, numbers[0] > 0 else { fail("bad crop \(args[3...7])") }
        let scale = Double(whole.width) / numbers[0]
        let rect = CGRect(x: numbers[1] * scale, y: numbers[2] * scale,
                          width: numbers[3] * scale, height: numbers[4] * scale)
        guard let cropped = whole.cropping(to: rect) else {
            emit(["colors": 0])
            exit(0)
        }
        picture = cropped
    }
    let width = picture.width, height = picture.height
    guard let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                                  bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
        fail("cannot read \(args[2])")
    }
    context.draw(picture, in: CGRect(x: 0, y: 0, width: width, height: height))
    guard let data = context.data else { fail("cannot read \(args[2])") }
    let pixels = data.bindMemory(to: UInt32.self, capacity: width * height)
    var colors = Set<UInt32>()
    for index in 0..<(width * height) where pixels[index] >> 24 == 0xFF {
        colors.insert(pixels[index])
        if colors.count >= 100_000 { break }
    }
    emit(["colors": colors.count, "width": width, "height": height])
default:
    fail("unknown command \(args[1])")
}
