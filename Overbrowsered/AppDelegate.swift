//
//  Created by https://keybase.io/bmisiak on 18.05.20.
//  Copyright © 2018 bmisiak. All rights reserved.
//

import Cocoa

class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
	private static let excludedBrowserBundleIdentifiers: Set<String> = [
		"com.openai.codex",
		"com.googlecode.iterm2",
	]
	private var userExcludedBrowserBundleIdentifiers = UserDefaults.standard.stringArray(forKey: "excludedBrowserBundleIdentifiers") ?? [] {
		didSet { UserDefaults.standard.set(userExcludedBrowserBundleIdentifiers, forKey: "excludedBrowserBundleIdentifiers") }
	}

	var menubarIcon: NSStatusItem?

	var mostRecentlyUsedBrowser: Bundle? {
		willSet(newAppBundle) {
			if mostRecentlyUsedBrowser != newAppBundle {
				if let bundle = newAppBundle {
					//the new value appears to be a valid app bundle, let's save it:
					UserDefaults.standard.set(bundle.bundleURL, forKey: "mostRecentBrowserBundleUrl")
				}
			}
		}
	}

	func applicationWillFinishLaunching(_ notification: Notification) {

		// Load the saved most recently used browser...
		let savedMostRecentBrowserBundleUrl = UserDefaults.standard.url(forKey: "mostRecentBrowserBundleUrl")

		if let url = savedMostRecentBrowserBundleUrl {
			self.mostRecentlyUsedBrowser = Bundle(url: url)
		}

		// It might be possible to use CF to fetch a list of apps which are already
		// running, sorted by the time of last use: https://gist.github.com/0xced/163918
		// The API is deprecated though.

		// If there's no valid saved browser, try using the one currently set as default:
		if self.mostRecentlyUsedBrowser == nil {
			if let defaultBrowserUrl = NSWorkspace.shared.urlForApplication(toOpen: URL(string: "https://example.com")!) {

				let defaultBrowserBundle = Bundle(url: defaultBrowserUrl)
				// But only if the default handler is not Overbrowsered.
				if let defaultBrowserBundle, defaultBrowserBundle != Bundle.main, !self.isExcluded(defaultBrowserBundle) {
					self.mostRecentlyUsedBrowser = defaultBrowserBundle
				}
			}
		}

		// Let's let the OS know we want to handle http links. Note: this has to
		// happen before applicationDidFinishLaunching or a link clicked while the app
		// is closed won't be handled.

		//Registering the handler of http links, the official way:
		NSAppleEventManager.shared().setEventHandler(self, andSelector: #selector(self.handleHttpLink), forEventClass: AEEventClass(kInternetEventClass), andEventID: AEEventID(kAEGetURL))

		//Some apps use the alternative WWW!/OURL AppleEvent, so for compatibility:
		if let eventClass = AEEventClass("WWW!"), let eventId = AEEventID("OURL") {
			NSAppleEventManager.shared().setEventHandler(self, andSelector: #selector(self.handleHttpLink), forEventClass: eventClass, andEventID: eventId)
		}
	}

	func applicationDidFinishLaunching(_ aNotification: Notification) {

		self.menubarIcon = NSStatusBar.system.statusItem(withLength:NSStatusItem.squareLength)
		self.menubarIcon?.button?.image = NSImage(named: "StatusBarButtonImage")
		self.menubarIcon?.button?.setAccessibilityLabel("Overbrowsered")

		let menu = NSMenu()
		menu.delegate = self
		self.menubarIcon?.menu = menu

		//Subscribing to app change events, to detect the most recently used browser:
		NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: nil, using: self.handleAppChangeNotification)
	}

	public func menuNeedsUpdate(_ menu: NSMenu) {
		menu.removeAllItems()

		menu.addItem(withTitle: "Overbrowsered by @ibmisiak", action: #selector(self.openOverbrowseredWebsite), keyEquivalent: "")
		menu.addItem(NSMenuItem.separator())

		menu.addItem(withTitle: "Most recently used browser: \(self.browserName(self.mostRecentlyUsedBrowser) ?? "Unknown (use any browser to detect)")", action: nil, keyEquivalent: "")

		//Detect the default handler status
		var defaultBrowserBundle: Bundle?
		if let url = NSWorkspace.shared.urlForApplication(toOpen: URL(string: "https://example.com")!) {
			defaultBrowserBundle = Bundle(url: url)
		}

		if defaultBrowserBundle == Bundle.main {
			menu.addItem(withTitle: "Default http handler: me 👌", action: nil, keyEquivalent: "")
		} else if defaultBrowserBundle?.bundleIdentifier == Bundle.main.bundleIdentifier {
			menu.addItem(withTitle: "Default http handler: Overbrowsered", action: nil, keyEquivalent: "")
		} else {
			let handlerName = self.browserName(defaultBrowserBundle)

			menu.addItem(withTitle: "Default http handler: \(handlerName ?? "not me") ☹️", action: nil, keyEquivalent: "")
			menu.addItem(withTitle: "⚠️ For Overbrowsered to work, click here to set it as the default \"browser\".", action: #selector(self.menuBarSetDefault(_:)), keyEquivalent: "")
		}

		if self.mostRecentlyUsedBrowser != nil || !self.userExcludedBrowserBundleIdentifiers.isEmpty {
			menu.addItem(NSMenuItem.separator())

			if let browser = self.mostRecentlyUsedBrowser, !self.isExcluded(browser) {
				let item = menu.addItem(withTitle: "Stop treating \(self.browserName(browser) ?? "[name unknown]") as a browser…", action: #selector(self.excludeBrowser(_:)), keyEquivalent: "")
				item.representedObject = browser
			}

			for bundleIdentifier in self.userExcludedBrowserBundleIdentifiers {
				let item = menu.addItem(withTitle: "Unignore \(self.browserName(bundleIdentifier))...", action: #selector(self.restoreBrowser(_:)), keyEquivalent: "")
				item.representedObject = bundleIdentifier
			}
		}

		menu.addItem(NSMenuItem.separator())
		menu.addItem(NSMenuItem(title: "Quit", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
	}

	@objc private func excludeBrowser(_ sender: NSMenuItem) {
		guard let browser = sender.representedObject as? Bundle, let bundleIdentifier = browser.bundleIdentifier, let name = self.browserName(browser), self.confirm("Stop treating \(name) as a browser?", "Overbrowsered will stop opening links in \(name). You can undo this from the menu.", "Stop Treating as Browser") else { return }

		self.userExcludedBrowserBundleIdentifiers.append(bundleIdentifier)
		self.mostRecentlyUsedBrowser = nil
		UserDefaults.standard.removeObject(forKey: "mostRecentBrowserBundleUrl")
	}

	@objc private func restoreBrowser(_ sender: NSMenuItem) {
		guard let bundleIdentifier = sender.representedObject as? String, self.confirm("Treat \(self.browserName(bundleIdentifier)) as a browser again?", "Overbrowsered will go back to opening links in \(self.browserName(bundleIdentifier)) after you use it.", "Treat as Browser") else { return }

		self.userExcludedBrowserBundleIdentifiers.removeAll { $0 == bundleIdentifier }
	}

	private func confirm(_ message: String, _ information: String, _ button: String) -> Bool {
		let alert = NSAlert()
		alert.messageText = message
		alert.informativeText = information
		alert.addButton(withTitle: button)
		alert.addButton(withTitle: "Cancel")
		return alert.runModal() == .alertFirstButtonReturn
	}

	private func browserName(_ browser: Bundle?) -> String? {
		browser?.infoDictionary?["CFBundleName"] as? String ?? browser?.bundleIdentifier
	}

	private func browserName(_ bundleIdentifier: String) -> String {
		let bundle = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleIdentifier).flatMap(Bundle.init(url:))
		return self.browserName(bundle) ?? bundleIdentifier
	}

	private func isExcluded(_ browser: Bundle) -> Bool {
		guard let bundleIdentifier = browser.bundleIdentifier else { return false }
		return Self.excludedBrowserBundleIdentifiers.contains(bundleIdentifier) || self.userExcludedBrowserBundleIdentifiers.contains(bundleIdentifier)
	}

	@objc private func openOverbrowseredWebsite() {
		open([URL(string: "https://bmisiak.com/#overbrowsered")!], reportsErrors: true)
	}

	// Try setting this app as the default handler for http(s), per user request:

	@objc func menuBarSetDefault(_ sender: Any?) {
		guard Bundle.main.bundlePath.starts(with: "/Applications/") else {
			let alert = NSAlert.init()
			alert.alertStyle = .informational
			alert.addButton(withTitle: "OK")
			alert.messageText = "Please move Overbrowsered to your Applications folder."
			alert.informativeText = "You will run into issues if you move Overbrowsered after setting it as the default browser. Please move it into Applications beforehand."
			alert.runModal()
			return
		}

		Task { @MainActor in
			do {
				try await self.setDefaultApplication(scheme: "http")
				try await self.setDefaultApplication(scheme: "https")
			} catch {
				let alert = NSAlert()
				alert.alertStyle = .warning
				alert.addButton(withTitle: "OK")
				alert.messageText = "Overbrowsered couldn't become the default link handler."
				alert.informativeText = error.localizedDescription
				alert.runModal()
			}
		}
	}

	private func setDefaultApplication(scheme: String) async throws {
		try await NSWorkspace.shared.setDefaultApplication(at: Bundle.main.bundleURL, toOpenURLsWithScheme: scheme)
	}

	// The user brought another app to the foreground, let's see if it's a browser:

	func handleAppChangeNotification(notification: Notification) {
		guard let appPassedInNotification = notification.userInfo?["NSWorkspaceApplicationKey"] as? NSRunningApplication else { return }
		guard let appBundleUrl = appPassedInNotification.bundleURL else { return }
		guard let appBundle = Bundle(url: appBundleUrl) else { return }

		if appBundle == Bundle.main {
			//Let's avoid detecting this app as a browser, which could cause an infinite loop of passing http links to itself
			return
		}

		let appIsABrowser =
			(appBundle.infoDictionary?["CFBundleURLTypes"] as? [[String:Any?]])?
			.map { urlType in urlType["CFBundleURLSchemes"] as? [String?] }
			.contains { schemes in
				schemes?.contains { scheme in scheme == "http" || scheme == "https" } ?? false
			}
			?? false

		if appIsABrowser && !self.isExcluded(appBundle) {
			self.mostRecentlyUsedBrowser = appBundle
		}
	}

    func application(_ sender: NSApplication, openFiles filenames: [String]) {
		open(filenames.map { URL(fileURLWithPath: $0).standardized }, reportsErrors: false)
	}

	@objc func handleHttpLink(getUrl: NSAppleEventDescriptor, withReplyEvent: NSAppleEventDescriptor) {

		guard let urlStr = getUrl.paramDescriptor(forKeyword: keyDirectObject)?.stringValue else { return }
		guard let url = URL(string: urlStr) else { return }

		if mostRecentlyUsedBrowser != nil {
			open([url], reportsErrors: true)
		} else {
			let alert = NSAlert.init()
			let ignoredBrowsers = self.userExcludedBrowserBundleIdentifiers.map { "• \(self.browserName($0))" }
			alert.alertStyle = .informational
			alert.addButton(withTitle: "OK")
			alert.messageText = "Overbrowsered has yet to see you use a browser."
			alert.informativeText = "Open a web browser and click its window, so I can know where to open this link:\n\n\(urlStr)" + (ignoredBrowsers.isEmpty ? "" : "\n\nIgnored browsers:\n\(ignoredBrowsers.joined(separator: "\n"))")
			alert.runModal()
		}
	}

	private func open(_ urls: [URL], reportsErrors: Bool) {
		guard let browserBundleURL = mostRecentlyUsedBrowser?.bundleURL else { return }

		let configuration = NSWorkspace.OpenConfiguration()
		configuration.createsNewApplicationInstance = false
		configuration.requiresUniversalLinks = false

		NSWorkspace.shared.open(urls, withApplicationAt: browserBundleURL, configuration: configuration) { _, error in
			guard reportsErrors, let error else { return }

			DispatchQueue.main.async {
				let alert = NSAlert()
				alert.alertStyle = .warning
				alert.addButton(withTitle: "OK")
				alert.messageText = "Overbrowsered couldn't open the link."
				alert.informativeText = error.localizedDescription
				alert.runModal()
			}
		}
	}

}
