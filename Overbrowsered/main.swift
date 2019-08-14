//
//  main.swift
//
//  Created by https://keybase.io/bmisiak on 18.05.22.
//  Copyright © 2018 bmisiak. All rights reserved.
//

import Cocoa

let app = NSApplication.shared
let appDelegate = AppDelegate()
app.delegate = appDelegate
_ = NSApplicationMain(CommandLine.argc, CommandLine.unsafeArgv)
