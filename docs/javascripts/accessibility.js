(() => {
    "use strict";

    let keyboardMode = false;
    document.addEventListener("keydown", () => {
        keyboardMode = true;
    }, true);
    document.addEventListener("pointerdown", () => {
        keyboardMode = false;
    }, true);

    const focusControl = (element) => {
        if (keyboardMode) {
            element.dataset.sfKeyboardFocus = "true";
            element.addEventListener("blur", () => {
                delete element.dataset.sfKeyboardFocus;
            }, { once: true });
        }
        element.focus();
    };

    const isVisible = (element) => {
        const bounds = element.getBoundingClientRect();
        return bounds.width > 0 && bounds.height > 0;
    };

    const intersectsHorizontally = (element, container) => {
        const elementBounds = element.getBoundingClientRect();
        const containerBounds = container.getBoundingClientRect();
        return elementBounds.right > containerBounds.left
            && elementBounds.left < containerBounds.right;
    };

    const isCompact = () => window.matchMedia("(max-width: 44.9844em)").matches;

    const trapFocus = (container, isActive, activeScope = () => container) => {
        if (container.dataset.sfFocusTrap === "true") {
            return;
        }
        container.dataset.sfFocusTrap = "true";
        container.addEventListener("keydown", (event) => {
            if (event.key !== "Tab" || !isActive()) {
                return;
            }
            const scope = activeScope();
            const focusable = [
                ...scope.querySelectorAll("*"),
            ].filter((element) => (
                element.tabIndex >= 0
                && isVisible(element)
                && intersectsHorizontally(element, container)
                && !element.closest("[inert]")
            ));
            if (focusable.length === 0) {
                return;
            }
            const current = focusable.indexOf(document.activeElement);
            const offset = event.shiftKey ? -1 : 1;
            const origin = current >= 0 ? current : (event.shiftKey ? 0 : -1);
            const next = (origin + offset + focusable.length) % focusable.length;
            event.preventDefault();
            event.stopImmediatePropagation();
            focusControl(focusable[next]);
        }, true);
    };

    const bindButtonLabel = (label, name) => {
        label.setAttribute("role", "button");
        label.setAttribute("aria-label", name);
        label.tabIndex = 0;
        if (label.dataset.sfKeyboardButton === "true") {
            return;
        }
        label.dataset.sfKeyboardButton = "true";
        label.addEventListener("keydown", (event) => {
            // Material already delegates Enter activation for focusable labels.
            // Space needs the native-label click that ordinary buttons provide.
            if (event.key !== " ") {
                return;
            }
            event.preventDefault();
            label.click();
        });
    };

    const enhanceShell = () => {
        const drawer = document.querySelector("#__drawer");
        const drawerOpener = document.querySelector(
            'label.md-header__button[for="__drawer"]'
        );
        const drawerCloser = document.querySelector("button.sf-drawer-close");
        const sidebar = document.querySelector(".md-sidebar--primary");

        if (drawer && drawerOpener && drawerCloser && sidebar) {
            sidebar.id = "sf-primary-navigation";
            bindButtonLabel(drawerOpener, "Open primary navigation");
            drawerOpener.setAttribute("aria-controls", sidebar.id);
            drawerCloser.setAttribute("aria-controls", sidebar.id);

            if (drawerCloser.dataset.sfKeyboardButton !== "true") {
                drawerCloser.dataset.sfKeyboardButton = "true";
                drawerCloser.addEventListener("click", () => {
                    drawer.checked = false;
                    drawer.dispatchEvent(new Event("change", { bubbles: true }));
                });
            }
            if (sidebar.dataset.sfEscapeClose !== "true") {
                sidebar.dataset.sfEscapeClose = "true";
                sidebar.addEventListener("keydown", (event) => {
                    if (event.key === "Escape" && isCompact() && drawer.checked) {
                        event.preventDefault();
                        drawer.checked = false;
                        drawer.dispatchEvent(new Event("change", { bubbles: true }));
                    }
                });
            }
            const activeDrawerScope = () => {
                let active = sidebar;
                let activeDepth = -1;
                const labels = [
                    ...sidebar.querySelectorAll("label.md-nav__link[aria-controls]"),
                ];
                for (const panel of sidebar.querySelectorAll("nav.md-nav[id]")) {
                    const label = labels.find(
                        (candidate) => candidate.getAttribute("aria-controls") === panel.id
                    );
                    const toggle = label && document.getElementById(label.htmlFor);
                    let depth = 0;
                    for (
                        let parent = panel.parentElement;
                        parent && sidebar.contains(parent);
                        parent = parent.parentElement
                    ) {
                        depth += 1;
                    }
                    if (toggle?.checked && !panel.inert && depth >= activeDepth) {
                        active = panel;
                        activeDepth = depth;
                    }
                }
                return active;
            };
            trapFocus(
                sidebar,
                () => isCompact() && drawer.checked,
                activeDrawerScope
            );

            const syncDrawer = () => {
                const compact = isCompact();
                const expanded = compact && drawer.checked;
                drawerOpener.setAttribute("aria-expanded", String(expanded));
                drawerCloser.setAttribute("aria-expanded", String(expanded));
                if (expanded) {
                    sidebar.setAttribute("role", "dialog");
                    sidebar.setAttribute("aria-modal", "true");
                    sidebar.setAttribute("aria-label", "Primary navigation");
                } else {
                    sidebar.removeAttribute("role");
                    sidebar.removeAttribute("aria-modal");
                    sidebar.removeAttribute("aria-label");
                }
                drawerOpener.setAttribute(
                    "aria-label",
                    expanded ? "Close primary navigation" : "Open primary navigation"
                );
                drawerCloser.setAttribute("aria-label", "Close primary navigation");
                const focusWasInSidebar = sidebar.contains(document.activeElement);
                sidebar.inert = compact && !expanded;
                if (sidebar.inert) {
                    sidebar.setAttribute("aria-hidden", "true");
                } else {
                    sidebar.removeAttribute("aria-hidden");
                }

                if (expanded) {
                    requestAnimationFrame(() => {
                        if (!sidebar.contains(document.activeElement)) {
                            const scope = activeDrawerScope();
                            const back = scope === sidebar
                                ? null
                                : scope.querySelector(
                                    ':scope > label.md-nav__title[role="button"]'
                                );
                            focusControl(back || drawerCloser);
                        }
                    });
                } else if (!expanded && focusWasInSidebar) {
                    if (compact) {
                        focusControl(drawerOpener);
                    } else if (!isVisible(document.activeElement)) {
                        const home = document.querySelector(".md-header__button.md-logo");
                        if (home) {
                            focusControl(home);
                        }
                    }
                }
                if (
                    !compact
                    && drawer.checked
                    && (
                        document.activeElement === document.body
                        || !isVisible(document.activeElement)
                    )
                ) {
                    const home = document.querySelector(".md-header__button.md-logo");
                    if (home) {
                        focusControl(home);
                    }
                }
            };

            if (drawer.dataset.sfAccessibilitySync !== "true") {
                drawer.dataset.sfAccessibilitySync = "true";
                drawer.addEventListener("change", syncDrawer);
            }
            syncDrawer();
        }

        const sectionLabels = [
            ...document.querySelectorAll("label.md-nav__link[for]"),
        ];
        for (const label of sectionLabels) {
            const toggle = document.getElementById(label.htmlFor);
            if (!toggle || toggle.type !== "checkbox") {
                continue;
            }
            const controlledPanel = [
                ...document.querySelectorAll("nav.md-nav"),
            ].find((panel) => panel.getAttribute("aria-labelledby") === label.id)
                || label.closest("li")?.querySelector("nav.md-nav--secondary");
            if (!controlledPanel) {
                continue;
            }

            if (!controlledPanel.id) {
                controlledPanel.id = `sf-panel-${toggle.id.replace(/^__/, "")}`;
            }
            const labelName = label.textContent.trim()
                || label.parentElement?.querySelector("a.md-nav__link")?.textContent.trim()
                || "Toggle navigation section";
            bindButtonLabel(label, labelName);
            label.setAttribute("aria-controls", controlledPanel.id);

            const backLabel = controlledPanel.querySelector(
                `:scope > label.md-nav__title[for="${toggle.id}"]`
            );
            if (backLabel) {
                bindButtonLabel(backLabel, `Back from ${labelName}`);
                backLabel.setAttribute("aria-controls", controlledPanel.id);
            }

            const syncSection = () => {
                const expanded = !isCompact() || toggle.checked;
                label.setAttribute("aria-expanded", String(expanded));
                backLabel?.setAttribute("aria-expanded", String(expanded));
                const focusWasInPanel = controlledPanel.contains(document.activeElement);
                controlledPanel.inert = !expanded;
                if (controlledPanel.inert) {
                    controlledPanel.setAttribute("aria-hidden", "true");
                } else {
                    controlledPanel.removeAttribute("aria-hidden");
                }
                if (!expanded && focusWasInPanel) {
                    focusControl(label);
                } else if (
                    isCompact()
                    && toggle.checked
                    && document.activeElement === label
                    && backLabel
                ) {
                    requestAnimationFrame(() => focusControl(backLabel));
                }
            };
            if (toggle.dataset.sfAccessibilitySync !== "true") {
                toggle.dataset.sfAccessibilitySync = "true";
                toggle.addEventListener("change", syncSection);
            }
            syncSection();
        }

        const search = document.querySelector("#__search");
        const searchButton = document.querySelector(
            'label.md-header__button[for="__search"]'
        );
        const searchDialog = document.querySelector(".md-search");
        if (search && searchButton && searchDialog) {
            searchDialog.id = "sf-documentation-search";
            searchDialog.setAttribute("aria-label", "Search documentation");
            bindButtonLabel(searchButton, "Search documentation");
            searchButton.setAttribute("aria-controls", searchDialog.id);
            trapFocus(
                searchDialog,
                () => isCompact() && search.checked
            );
            const syncSearch = () => {
                const compact = isCompact();
                const expanded = compact && search.checked;
                searchButton.setAttribute("aria-expanded", String(expanded));
                if (expanded) {
                    searchDialog.setAttribute("aria-modal", "true");
                } else {
                    searchDialog.removeAttribute("aria-modal");
                }
                const focusWasInSearch = searchDialog.contains(document.activeElement);
                searchDialog.inert = compact && !expanded;
                if (searchDialog.inert) {
                    searchDialog.setAttribute("aria-hidden", "true");
                } else {
                    searchDialog.removeAttribute("aria-hidden");
                }
                if (expanded) {
                    requestAnimationFrame(() => {
                        const input = searchDialog.querySelector("input");
                        if (input && !searchDialog.contains(document.activeElement)) {
                            focusControl(input);
                        }
                    });
                } else if (!expanded && focusWasInSearch) {
                    if (compact) {
                        focusControl(searchButton);
                    } else if (!isVisible(document.activeElement)) {
                        const home = document.querySelector(".md-header__button.md-logo");
                        if (home) {
                            focusControl(home);
                        }
                    }
                }
                if (
                    !compact
                    && search.checked
                    && (
                        document.activeElement === document.body
                        || !isVisible(document.activeElement)
                    )
                ) {
                    const input = searchDialog.querySelector("input");
                    if (input) {
                        focusControl(input);
                    }
                }
            };
            if (search.dataset.sfAccessibilitySync !== "true") {
                search.dataset.sfAccessibilitySync = "true";
                search.addEventListener("change", syncSearch);
            }
            syncSearch();
        }

        const paletteLabels = [
            ...document.querySelectorAll('label.md-header__button[for^="__palette_"]'),
        ];
        for (const label of paletteLabels) {
            bindButtonLabel(label, label.title || "Change color scheme");
            const radio = document.querySelector(`#${label.htmlFor}`);
            if (radio) {
                radio.tabIndex = -1;
                if (radio.dataset.sfAccessibilitySync !== "true") {
                    radio.dataset.sfAccessibilitySync = "true";
                    radio.addEventListener("change", () => {
                        requestAnimationFrame(() => {
                            requestAnimationFrame(() => {
                                const visible = paletteLabels.find(isVisible);
                                if (visible) {
                                    focusControl(visible);
                                }
                            });
                        });
                    });
                }
            }
        }

        console.assert(
            !document.querySelector(
                '[role="button"] a, [role="button"] button, [role="button"] input, [role="button"] select, [role="button"] textarea'
            ),
            "Button-role controls must not contain interactive descendants"
        );
    };

    enhanceShell();
    let resizeFrame;
    window.addEventListener("resize", () => {
        cancelAnimationFrame(resizeFrame);
        resizeFrame = requestAnimationFrame(enhanceShell);
    });
    if (typeof document$ !== "undefined") {
        document$.subscribe(enhanceShell);
    }
})();
