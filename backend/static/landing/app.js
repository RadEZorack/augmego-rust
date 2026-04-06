const PRIMARY_CTA_ID = "landing-primary-cta";
const SECONDARY_CTA_ID = "landing-secondary-cta";
const SCENE_CONTAINER_ID = "landing-scene";
const LANDING_EVENT_ENDPOINT = "/api/v1/landing/event";

let pageViewTracked = false;
let sceneBootRequested = false;

function trackLandingEvent(eventName) {
  const body = JSON.stringify({ event: eventName });

  if (navigator.sendBeacon) {
    const queued = navigator.sendBeacon(
      LANDING_EVENT_ENDPOINT,
      new Blob([body], { type: "application/json" }),
    );
    if (queued) {
      return;
    }
  }

  fetch(LANDING_EVENT_ENDPOINT, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body,
    keepalive: true,
  }).catch(() => {
    // Landing analytics are best-effort and should never interrupt navigation.
  });
}

function bindTrackedLink(elementId, eventName) {
  const element = document.getElementById(elementId);
  if (!element) {
    return;
  }

  element.addEventListener("click", () => {
    trackLandingEvent(eventName);
  });
}

function scheduleIdle(task) {
  if ("requestIdleCallback" in window) {
    window.requestIdleCallback(task, { timeout: 1600 });
    return;
  }

  window.setTimeout(task, 180);
}

async function bootScene() {
  if (sceneBootRequested) {
    return;
  }
  sceneBootRequested = true;

  const container = document.getElementById(SCENE_CONTAINER_ID);
  if (!container) {
    return;
  }

  const prefersReducedMotion = window.matchMedia?.(
    "(prefers-reduced-motion: reduce)",
  )?.matches;
  if (prefersReducedMotion) {
    document.body.classList.add("scene-disabled");
    document.body.dataset.sceneState = "unavailable";
    return;
  }

  document.body.dataset.sceneState = "loading";

  try {
    const module = await import("/landing/scene.js");
    const result = await module.bootLandingScene({
      container,
      onLoaded() {
        document.body.dataset.sceneState = "ready";
        trackLandingEvent("scene_loaded");
      },
      onUnavailable() {
        document.body.dataset.sceneState = "unavailable";
      },
    });

    if (!result?.available) {
      document.body.dataset.sceneState = "unavailable";
    }
  } catch (error) {
    console.warn("landing scene unavailable", error);
    document.body.dataset.sceneState = "failed";
  }
}

function main() {
  bindTrackedLink(PRIMARY_CTA_ID, "primary_cta_click");
  bindTrackedLink(SECONDARY_CTA_ID, "secondary_cta_click");

  if (!pageViewTracked) {
    pageViewTracked = true;
    trackLandingEvent("page_view");
  }

  window.requestAnimationFrame(() => {
    scheduleIdle(() => {
      void bootScene();
    });
  });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", main, { once: true });
} else {
  main();
}
