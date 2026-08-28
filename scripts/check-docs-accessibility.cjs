"use strict";

const fs = require("node:fs/promises");
const http = require("node:http");
const path = require("node:path");
const { chromium } = require("playwright");

const host = "127.0.0.1";
const repoRoot = path.resolve(__dirname, "..");

// Generous enough for the slowest observed hosted runner (healthy passes stay
// under 180 seconds; one slow runner exceeded 300) while far below the job's
// 20-minute ceiling, so a genuine hang still fails fast.
const ACCESSIBILITY_BUDGET_MS = 600_000;

let origin;
let activeBrowser;
let activeServer;
let cleanupPromise;
let timedOut = false;

const expectState = (condition, message, evidence) => {
    if (!condition) {
        throw new Error(`${message}: ${JSON.stringify(evidence)}`);
    }
};

const assertBuildFreshness = async () => {
    for (const relative of [
        "javascripts/accessibility.js",
        "stylesheets/extra.css",
    ]) {
        const [source, built] = await Promise.all([
            fs.readFile(path.join(repoRoot, "docs", relative)),
            fs.readFile(path.join(repoRoot, "site", relative)),
        ]);
        expectState(
            source.equals(built),
            `site/${relative} is stale; run mkdocs build --strict first`,
            { relative }
        );
    }
};

const startServer = async () => {
    const siteRoot = await fs.realpath(path.join(repoRoot, "site"));
    const contentTypes = new Map([
        [".css", "text/css; charset=utf-8"],
        [".html", "text/html; charset=utf-8"],
        [".js", "text/javascript; charset=utf-8"],
        [".json", "application/json"],
        [".png", "image/png"],
        [".svg", "image/svg+xml"],
        [".txt", "text/plain; charset=utf-8"],
        [".woff2", "font/woff2"],
    ]);
    return new Promise((resolve, reject) => {
        const server = http.createServer(async (request, response) => {
            try {
                const pathname = decodeURIComponent(
                    new URL(request.url, "http://localhost").pathname
                );
                let filePath = path.resolve(siteRoot, `.${pathname}`);
                if (
                    filePath !== siteRoot
                    && !filePath.startsWith(`${siteRoot}${path.sep}`)
                ) {
                    response.writeHead(403).end();
                    return;
                }
                if ((await fs.stat(filePath)).isDirectory()) {
                    filePath = path.join(filePath, "index.html");
                }
                const canonicalPath = await fs.realpath(filePath);
                if (
                    canonicalPath !== siteRoot
                    && !canonicalPath.startsWith(`${siteRoot}${path.sep}`)
                ) {
                    response.writeHead(403).end();
                    return;
                }
                const content = await fs.readFile(canonicalPath);
                response.writeHead(200, {
                    "Content-Type": contentTypes.get(path.extname(canonicalPath))
                        || "application/octet-stream",
                });
                response.end(content);
            } catch (error) {
                response.writeHead(error.code === "ENOENT" ? 404 : 500).end();
            }
        });
        server.once("error", reject);
        server.listen(Number(process.env.DOCS_A11Y_PORT || 0), host, () => {
            const address = server.address();
            origin = `http://${host}:${address.port}`;
            resolve(server);
        });
    });
};

const closeServer = (server) => new Promise((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
});

const cleanup = () => {
    const previousCleanup = cleanupPromise?.catch(() => undefined)
        || Promise.resolve();
    cleanupPromise = previousCleanup.then(async () => {
        const browser = activeBrowser;
        const server = activeServer;
        activeBrowser = undefined;
        activeServer = undefined;
        try {
            await browser?.close();
        } finally {
            if (server) {
                if (timedOut) {
                    server.closeAllConnections();
                }
                await closeServer(server);
            }
        }
    });
    return cleanupPromise;
};

// The deadline races the browser flow, so when it fires, its clear timeout
// error deterministically wins the report and cleanup cannot mask it with a
// mid-evaluate "Target crashed" from the closing browser.
const accessibilityDeadline = () => {
    let releaseDeadline;
    const deadline = new Promise((_, reject) => {
        const timer = setTimeout(() => {
            timedOut = true;
            reject(new Error(
                `Documentation accessibility checks exceeded ${ACCESSIBILITY_BUDGET_MS / 1000} seconds.`
            ));
        }, ACCESSIBILITY_BUDGET_MS);
        timer.unref();
        releaseDeadline = () => clearTimeout(timer);
    });
    return [deadline, () => releaseDeadline()];
};

const settleShell = async (page) => {
    // The browser context requests reduced motion, so CSS transitions finish in
    // 0.01ms. This bounded delay still gives resize/change handlers time to run
    // their scheduled animation frame without relying on a throttled rAF or on
    // document.getAnimations(), both of which are flaky in headless Chromium.
    await page.waitForTimeout(75);
};

const configurePage = (page) => {
    page.setDefaultTimeout(5000);
    page.setDefaultNavigationTimeout(15000);
    return page;
};

const attachErrorCapture = (page, errors) => {
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
        if (
            message.type() === "error"
            && !message.text().startsWith("Failed to load resource:")
        ) {
            errors.push(message.text());
        }
    });
    page.on("response", (response) => {
        if (response.url().startsWith(origin) && response.status() >= 400) {
            errors.push(`${response.status()} ${response.url()}`);
        }
    });
};

const drawerFocusState = (page) => page.evaluate(() => {
    const sidebar = document.querySelector(".md-sidebar--primary");
    const scrollwrap = sidebar.querySelector(".md-sidebar__scrollwrap");
    const toc = sidebar.querySelector(".md-nav--secondary");
    const focused = document.activeElement;
    const sidebarBounds = sidebar.getBoundingClientRect();
    const focusBounds = focused.getBoundingClientRect();
    const tocBounds = toc.getBoundingClientRect();
    return {
        drawerChecked: document.querySelector("#__drawer").checked,
        tocChecked: document.querySelector("#__toc").checked,
        modal: sidebar.getAttribute("aria-modal"),
        inert: sidebar.inert,
        scrollLeft: scrollwrap.scrollLeft,
        maxScroll: scrollwrap.scrollWidth - scrollwrap.clientWidth,
        tocVisible: tocBounds.width > 0 && tocBounds.height > 0,
        focusLabel: focused.getAttribute("aria-label"),
        focusInside: sidebar.contains(focused),
        focusVisible: focusBounds.width > 0 && focusBounds.height > 0,
        focusContained: focusBounds.left >= sidebarBounds.left - 0.5
            && focusBounds.right <= sidebarBounds.right + 0.5,
        focusInert: Boolean(focused.closest("[inert]")),
        sidebarBounds: [sidebarBounds.left, sidebarBounds.right],
        focusBounds: [focusBounds.left, focusBounds.right],
        tocBounds: [tocBounds.left, tocBounds.right],
    };
});

const assertTrappedDrawerFocus = async (page, keys, context) => {
    for (const key of keys) {
        await page.keyboard.press(key);
        const state = await drawerFocusState(page);
        expectState(
            state.focusInside
                && state.focusVisible
                && state.focusContained
                && !state.focusInert,
            `${context}: ${key} left a fully visible drawer target`,
            state
        );
    }
};

const hasValidDrawerFocus = (state) => (
    state.focusInside
    && state.focusVisible
    && state.focusContained
    && !state.focusInert
);

const searchFocusState = (page) => page.evaluate(() => {
    const search = document.querySelector(".md-search");
    const focused = document.activeElement;
    const bounds = focused.getBoundingClientRect();
    return {
        checked: document.querySelector("#__search").checked,
        modal: search.getAttribute("aria-modal"),
        inert: search.inert,
        focusInside: search.contains(focused),
        focusVisible: bounds.width > 0 && bounds.height > 0,
        focusInViewport: bounds.left >= -0.5
            && bounds.right <= window.innerWidth + 0.5
            && bounds.top >= -0.5
            && bounds.bottom <= window.innerHeight + 0.5,
        focusInert: Boolean(focused.closest("[inert]")),
        focusLabel: focused.getAttribute("aria-label"),
        focusBounds: [bounds.left, bounds.top, bounds.right, bounds.bottom],
    };
});

const hasValidSearchFocus = (state) => (
    state.focusInside
    && state.focusVisible
    && state.focusInViewport
    && !state.focusInert
);

const checkClosedBoundaries = async (page) => {
    const cases = [
        [719, true, true],
        [720, true, true],
        [800, true, true],
        [959, true, true],
        [960, true, false],
        [1100, true, false],
        [1219, true, false],
        [1220, false, false],
    ];
    await page.goto(origin, { waitUntil: "networkidle" });
    for (const [width, drawerOverlay, searchOverlay] of cases) {
        await page.setViewportSize({ width, height: 800 });
        await settleShell(page);
        const state = await page.evaluate(() => {
            const sidebar = document.querySelector(".md-sidebar--primary");
            const search = document.querySelector(".md-search");
            const opener = document.querySelector(
                'label.md-header__button[for="__drawer"]'
            );
            const sidebarBounds = sidebar.getBoundingClientRect();
            const openerBounds = opener.getBoundingClientRect();
            return {
                drawerInert: sidebar.inert,
                drawerHidden: sidebar.getAttribute("aria-hidden"),
                drawerBounds: [
                    sidebarBounds.left,
                    sidebarBounds.right,
                    sidebarBounds.width,
                ],
                openerVisible: openerBounds.width > 0 && openerBounds.height > 0,
                searchInert: search.inert,
                searchHidden: search.getAttribute("aria-hidden"),
                overflow: document.documentElement.scrollWidth
                    - document.documentElement.clientWidth,
            };
        });
        expectState(
            state.drawerInert === drawerOverlay
                && (state.drawerHidden === "true") === drawerOverlay
                && state.openerVisible === drawerOverlay
                && (
                    drawerOverlay
                        ? state.drawerBounds[1] <= 0.5
                        : state.drawerBounds[0] >= -0.5
                            && state.drawerBounds[2] > 0
                ),
            `drawer boundary mismatch at ${width}px`,
            state
        );
        expectState(
            state.searchInert === searchOverlay
                && (state.searchHidden === "true") === searchOverlay,
            `search boundary mismatch at ${width}px`,
            state
        );
        expectState(state.overflow <= 1, `document overflow at ${width}px`, state);
    }
};

const checkDrawerResize = async (page, direction) => {
    await page.setViewportSize({ width: 800, height: 800 });
    await page.goto(`${origin}/getting-started/`, { waitUntil: "networkidle" });
    await page.evaluate((dir) => {
        document.body.dir = dir;
    }, direction);

    await page.locator('label.md-header__button[for="__drawer"]').focus();
    await page.keyboard.press("Enter");
    await settleShell(page);
    await page.locator(
        '.md-sidebar--primary label.md-nav__link[for="__toc"]'
    ).focus();
    await page.keyboard.press("Enter");
    await settleShell(page);
    let state = await drawerFocusState(page);
    expectState(
        state.drawerChecked
            && state.tocChecked
            && state.tocVisible
            && state.modal === "true"
            && hasValidDrawerFocus(state),
        `${direction}: phone TOC did not open inside the drawer`,
        state
    );

    await page.setViewportSize({ width: 1100, height: 800 });
    await settleShell(page);
    state = await drawerFocusState(page);
    expectState(
        state.drawerChecked
            && state.tocChecked
            && !state.tocVisible
            && state.modal === "true"
            && Math.abs(state.scrollLeft) <= 0.5
            && state.focusLabel === "Back from Start Here"
            && hasValidDrawerFocus(state),
        `${direction}: tablet resize did not restore the root drawer geometry`,
        state
    );
    await assertTrappedDrawerFocus(
        page,
        [...Array(20).fill("Tab"), ...Array(20).fill("Shift+Tab")],
        `${direction}: tablet drawer`
    );

    await page.setViewportSize({ width: 1219, height: 800 });
    await settleShell(page);
    state = await drawerFocusState(page);
    expectState(
        state.drawerChecked
            && state.modal === "true"
            && Math.abs(state.scrollLeft) <= 0.5
            && hasValidDrawerFocus(state),
        `${direction}: upper drawer boundary lost geometry or focus`,
        state
    );
    await assertTrappedDrawerFocus(
        page,
        [...Array(10).fill("Tab"), ...Array(10).fill("Shift+Tab")],
        `${direction}: 1219px drawer`
    );
    await page.keyboard.press("Escape");
    await settleShell(page);
    state = await drawerFocusState(page);
    expectState(
        !state.drawerChecked
            && state.inert
            && state.focusLabel === "Open primary navigation",
        `${direction}: drawer Escape did not restore its opener`,
        state
    );
    await page.keyboard.press("Enter");
    await settleShell(page);
    state = await drawerFocusState(page);
    expectState(
        state.drawerChecked && state.modal === "true" && hasValidDrawerFocus(state),
        `${direction}: drawer did not reopen at 1219px`,
        state
    );

    await page.setViewportSize({ width: 800, height: 800 });
    await settleShell(page);
    state = await drawerFocusState(page);
    expectState(
        state.tocVisible
            && Math.abs(Math.abs(state.scrollLeft) - state.maxScroll) <= 0.5
            && state.focusLabel?.startsWith("Back from Installation")
            && hasValidDrawerFocus(state),
        `${direction}: phone resize did not restore the selected TOC geometry`,
        state
    );
    await assertTrappedDrawerFocus(
        page,
        [...Array(10).fill("Tab"), ...Array(10).fill("Shift+Tab")],
        `${direction}: restored phone TOC`
    );

    await page.setViewportSize({ width: 1280, height: 800 });
    await settleShell(page);
    state = await drawerFocusState(page);
    expectState(
        state.modal === null && !state.inert && state.focusVisible,
        `${direction}: desktop resize retained modal or hidden focus state`,
        state
    );
};

const checkSearchResize = async (page) => {
    await page.setViewportSize({ width: 959, height: 800 });
    await page.goto(origin, { waitUntil: "networkidle" });
    await page.locator('label.md-header__button[for="__search"]').focus();
    await page.keyboard.press("Enter");
    await settleShell(page);
    let state = await searchFocusState(page);
    expectState(
        state.checked
            && state.modal === "true"
            && !state.inert
            && hasValidSearchFocus(state),
        "959px search did not open as a modal",
        state
    );
    for (const key of [...Array(10).fill("Tab"), ...Array(10).fill("Shift+Tab")]) {
        await page.keyboard.press(key);
        state = await searchFocusState(page);
        expectState(
            hasValidSearchFocus(state),
            `959px search trap failed on ${key}`,
            state
        );
    }

    await page.setViewportSize({ width: 960, height: 800 });
    await settleShell(page);
    state = await searchFocusState(page);
    expectState(
        state.checked
            && state.modal === null
            && !state.inert
            && state.focusVisible
            && state.focusInViewport
            && !state.focusInert,
        "960px search did not become a usable non-modal control",
        state
    );

    await page.setViewportSize({ width: 959, height: 800 });
    await settleShell(page);
    state = await searchFocusState(page);
    expectState(
        state.checked
            && state.modal === "true"
            && !state.inert
            && hasValidSearchFocus(state),
        "959px search did not return to a usable modal",
        state
    );
    await page.keyboard.press("Escape");
    await settleShell(page);
    state = await searchFocusState(page);
    expectState(
        !state.checked && state.inert && state.focusLabel === "Search documentation",
        "search Escape did not restore its trigger",
        state
    );
};

const runBrowserChecks = async () => {
    const startedAt = Date.now();
    const elapsed = () => `${((Date.now() - startedAt) / 1000).toFixed(1)}s`;
    // Progress lines make a budget timeout diagnosable: the log names the
    // phase that was still running when the deadline fired.
    const runPhase = async (name, check) => {
        process.stderr.write(`Accessibility: start ${name} at ${elapsed()}\n`);
        await check();
        process.stderr.write(`Accessibility: done ${name} at ${elapsed()}\n`);
    };
    await runPhase("build freshness", assertBuildFreshness);
    const server = await startServer();
    if (timedOut) {
        // The deadline fired while startup was pending: cleanup has been
        // initiated and this resource was never assigned to a global, so
        // close it here or it is orphaned.
        await closeServer(server);
        throw new Error("Documentation accessibility checks timed out");
    }
    activeServer = server;
    const browserErrors = [];
    const browser = await chromium.launch({
        args: ["--disable-gpu"],
        headless: true,
    });
    if (timedOut) {
        await browser.close();
        throw new Error("Documentation accessibility checks timed out");
    }
    activeBrowser = browser;
    const context = await browser.newContext({
        reducedMotion: "reduce",
        viewport: { width: 1220, height: 800 },
    });
    const page = configurePage(await context.newPage());
    attachErrorCapture(page, browserErrors);
    await runPhase("closed boundaries", () => checkClosedBoundaries(page));
    await runPhase("drawer resize ltr", () => checkDrawerResize(page, "ltr"));
    await runPhase("drawer resize rtl", () => checkDrawerResize(page, "rtl"));
    await runPhase("search resize", () => checkSearchResize(page));
    expectState(
        browserErrors.length === 0,
        "documentation pages emitted browser errors",
        browserErrors
    );
    process.stdout.write("Documentation accessibility browser checks passed.\n");
};

(async () => {
    const [deadline, releaseDeadline] = accessibilityDeadline();
    let flowError;
    try {
        await Promise.race([runBrowserChecks(), deadline]);
    } catch (error) {
        flowError = error;
    } finally {
        releaseDeadline();
        try {
            await cleanup();
        } catch (cleanupError) {
            // Never let cleanup noise mask the primary (timeout) error.
            process.stderr.write(`Accessibility cleanup failed: ${cleanupError.message}\n`);
            flowError = flowError || cleanupError;
        }
    }
    if (flowError) {
        throw flowError;
    }
})().catch((error) => {
    process.stderr.write(`${error.stack || error.message}\n`);
    process.exitCode = 1;
});
