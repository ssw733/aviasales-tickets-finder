const state = {
  origin: { mode: "place", point: null, radius: 150, places: [], excluded: [] },
  destination: { mode: "place", point: null, radius: 150, places: [], excluded: [] },
  pickMode: null,
  itineraries: [],
  groups: { price: [], transfers: [], dates: [], duration: [] },
  activeGroup: "price",
  activeIndex: -1,
  map: null,
  originCircle: null,
  destinationCircle: null,
  originPreviewLayer: null,
  destinationPreviewLayer: null,
  searchLayer: null,
  routeLayer: null,
  previewTimers: {},
};

const els = {};

function $(id) {
  return document.getElementById(id);
}

function esc(value) {
  return String(value == null ? "" : value).replace(/[&<>"']/g, (ch) => {
    return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[ch];
  });
}

function safeUrl(value) {
  try {
    const url = new URL(value);
    if (url.protocol === "https:" || url.protocol === "http:") {
      return url.href;
    }
  } catch (error) {
    return "";
  }
  return "";
}

function numberValue(id) {
  const value = Number.parseInt($(id).value, 10);
  return Number.isFinite(value) ? value : 0;
}

function plural(value, forms) {
  const n = Math.abs(value) % 100;
  const n1 = n % 10;
  if (n > 10 && n < 20) return forms[2];
  if (n1 > 1 && n1 < 5) return forms[1];
  if (n1 === 1) return forms[0];
  return forms[2];
}

function transfersText(value) {
  if (value === 0) return "без пересадок";
  return `${value} ${plural(value, ["пересадка", "пересадки", "пересадок"])}`;
}

function minutesHuman(value) {
  const minutes = Math.max(0, Math.round(value || 0));
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  if (hours === 0) return `${rest} мин`;
  if (rest === 0) return `${hours} ч`;
  return `${hours} ч ${rest} мин`;
}

function priceHuman(value) {
  return new Intl.NumberFormat("ru-RU", { maximumFractionDigits: 0 }).format(Math.round(value || 0));
}

function dateParts(iso) {
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(String(iso || ""));
  if (!match) return { date: iso || "—", time: "—" };
  const months = ["янв", "фев", "мар", "апр", "мая", "июн", "июл", "авг", "сен", "окт", "ноя", "дек"];
  return {
    date: `${match[3]} ${months[Number(match[2]) - 1]}`,
    time: `${match[4]}:${match[5]}`,
  };
}

function setStatus(message, kind) {
  const status = $("status");
  status.textContent = message || "";
  status.className = "status" + (kind ? ` ${kind}` : "");
}

function clearLayer(layer) {
  if (layer) layer.clearLayers();
}

function updatePickButtons() {
  document.querySelectorAll(".pick-button").forEach((button) => {
    button.classList.toggle("active", state.pickMode === button.dataset.pick);
  });
}

function colorFor(which) {
  return which === "origin" ? "#2563eb" : "#dc2626";
}

function fillColorFor(which) {
  return which === "origin" ? "#3b82f6" : "#ef4444";
}

function drawCircle(which) {
  const item = state[which];
  if (!item.point) return;
  const key = which === "origin" ? "originCircle" : "destinationCircle";
  if (state[key]) {
    state.map.removeLayer(state[key]);
    state[key] = null;
  }
  const radius = Math.max(1000, item.radius * 1000);
  const circle = L.circle([item.point.lat, item.point.lon], {
    radius,
    color: colorFor(which),
    weight: 2,
    fillColor: fillColorFor(which),
    fillOpacity: 0.08,
    opacity: 0.9,
  }).addTo(state.map);
  state[key] = circle;
}

function removeCircle(which) {
  const key = which === "origin" ? "originCircle" : "destinationCircle";
  if (state[key]) {
    state.map.removeLayer(state[key]);
    state[key] = null;
  }
}

function updateCoords(which) {
  const item = state[which];
  const target = $(`${which}-coords`);
  if (!item.point) {
    target.textContent = "Точка не выбрана";
    return;
  }
  target.textContent = `${item.point.lat.toFixed(3)}, ${item.point.lon.toFixed(3)} · ${item.radius} км`;
}

function setRegionPoint(which, lat, lon) {
  state[which].point = { lat, lon };
  updateCoords(which);
  drawCircle(which);
  debouncePreview(which);
}

function syncRadius(which, value) {
  const radius = Math.max(5, Math.min(5000, Number(value) || 150));
  state[which].radius = radius;
  $(`${which}-radius`).value = String(radius);
  $(`${which}-radius-num`).value = String(radius);
  $(`${which}-radius-value`).textContent = `${radius} км`;
  if (state[which].point) {
    updateCoords(which);
    drawCircle(which);
    debouncePreview(which);
  }
}

function updateModeUI(which) {
  const mode = $(`${which}-mode`).value;
  state[which].mode = mode;
  const cityWrap = $(`${which}-city-wrap`);
  const placeWrap = $(`${which}-country-wrap`);
  const regionWrap = $(`${which}-region-wrap`);
  const anyNote = $(`${which}-any-note`);
  cityWrap.classList.add("hidden");
  placeWrap.classList.toggle("hidden", mode !== "place");
  regionWrap.classList.toggle("hidden", mode !== "region");
  anyNote.classList.toggle("hidden", mode !== "any");
  if (mode !== "region") {
    removeCircle(which);
    clearLayer(state[which === "origin" ? "originPreviewLayer" : "destinationPreviewLayer"]);
    $(`${which}-region-cities`).innerHTML = "";
  } else {
    if (state[which].point) {
      updateCoords(which);
      drawCircle(which);
    } else {
      $(`${which}-coords`).textContent = "Кликните «Указать на карте»";
    }
  }
  if (mode === "any") {
    state.pickMode = state.pickMode === which ? null : state.pickMode;
    updatePickButtons();
  }
}

function debouncePreview(which) {
  clearTimeout(state.previewTimers[which]);
  state.previewTimers[which] = setTimeout(() => previewRegion(which), 350);
}

async function previewRegion(which) {
  const item = state[which];
  const layer = state[which === "origin" ? "originPreviewLayer" : "destinationPreviewLayer"];
  const chips = $(`${which}-region-cities`);
  if (item.mode !== "region" || !item.point) {
    clearLayer(layer);
    chips.innerHTML = "";
    return;
  }
  try {
    const url = `/api/region?lat=${encodeURIComponent(item.point.lat)}&lon=${encodeURIComponent(item.point.lon)}&radius_km=${encodeURIComponent(item.radius)}&limit=50`;
    const response = await fetch(url);
    if (!response.ok) throw new Error("region");
    const cities = await response.json();
    chips.innerHTML = cities
      .map((city) => `<span class="chip" title="${esc(city.name)}">${esc(city.code)}</span>`)
      .join("");
    clearLayer(layer);
    cities.forEach((city) => {
      L.circleMarker([city.lat, city.lon], {
        radius: 4,
        color: "#fff",
        weight: 1,
        fillColor: fillColorFor(which),
        fillOpacity: 0.9,
      })
        .bindTooltip(`${city.code} · ${city.name}`)
        .addTo(layer);
    });
  } catch (error) {
    chips.innerHTML = "";
    clearLayer(layer);
  }
}

function bindCityAutocomplete(inputId) {
  const input = $(inputId);
  let timer = null;
  input.addEventListener("input", () => {
    clearTimeout(timer);
    const query = input.value.trim();
    if (query.length < 2) return;
    timer = setTimeout(async () => {
      try {
        const response = await fetch(`/api/cities?q=${encodeURIComponent(query)}&limit=15`);
        if (!response.ok) return;
        const cities = await response.json();
        $("city-list").innerHTML = cities
          .map(
            (city) =>
              `<option value="${esc(city.code)}">${esc(city.code)} — ${esc(city.name)}</option>`
          )
          .join("");
      } catch (error) {
        return;
      }
    }, 220);
  });
}

function bindPlaceAutocomplete(inputId) {
  const input = $(inputId);
  let timer = null;
  input.addEventListener("input", () => {
    clearTimeout(timer);
    const query = input.value.trim();
    if (query.length < 2) return;
    timer = setTimeout(async () => {
      try {
        const response = await fetch(`/api/places?q=${encodeURIComponent(query)}&limit=20`);
        if (!response.ok) return;
        const places = await response.json();
        $("country-list").innerHTML = places
          .map(
            (place) =>
              `<option value="${esc(place.code)}">${esc(place.code)} — ${esc(place.name)}${
                place.kind === "country" ? " (страна)" : ""
              }</option>`
          )
          .join("");
      } catch (error) {
        return;
      }
    }, 220);
  });
}

function renderPlaceChips(side) {
  const target = $(`${side}-country-chips`);
  const places = state[side].places;
  target.innerHTML = places
    .map(
      (place) =>
        `<button type="button" class="chip country-chip" data-side="${side}" data-code="${esc(
          place.code
        )}" title="Удалить">${esc(place.code)} · ${esc(place.name)} ×</button>`
    )
    .join("");
  target.querySelectorAll(".country-chip").forEach((button) => {
    button.addEventListener("click", () => {
      state[side].places = state[side].places.filter(
        (place) => place.code !== button.dataset.code
      );
      renderPlaceChips(side);
    });
  });
}

async function addPlacesFromInput(side) {
  const input = $(`${side}-country`);
  const parts = input.value
    .split(/[,\n]+/)
    .map((value) => value.trim())
    .filter(Boolean);
  if (parts.length === 0) return;
  const errors = [];
  for (const part of parts) {
    try {
      const response = await fetch(
        `/api/places?q=${encodeURIComponent(part)}&limit=20`
      );
      if (!response.ok) {
        errors.push(part);
        continue;
      }
      const items = await response.json();
      let place = items.find(
        (item) => item.code.toUpperCase() === part.toUpperCase()
      );
      if (!place) {
        place = items.find(
          (item) => item.name.toLowerCase() === part.toLowerCase()
        );
      }
      if (!place) place = items[0];
      if (!place) {
        errors.push(part);
        continue;
      }
      if (
        !state[side].places.some(
          (item) => item.code === place.code && item.kind === place.kind
        )
      ) {
        state[side].places.push({
          kind: place.kind,
          code: place.code,
          name: place.name,
        });
      }
    } catch (error) {
      errors.push(part);
    }
  }
  input.value = "";
  renderPlaceChips(side);
  if (errors.length > 0) {
    setStatus(`Не найдено: ${errors.join(", ")}`, "error");
  }
}

function renderExcludeChips(side) {
  const target = $(`${side}-exclude-chips`);
  const places = state[side].excluded;
  target.innerHTML = places
    .map(
      (place) =>
        `<button type="button" class="chip exclude-chip" data-side="${side}" data-code="${esc(
          place.code
        )}" title="Удалить">− ${esc(place.code)} · ${esc(place.name)} ×</button>`
    )
    .join("");
  target.querySelectorAll(".exclude-chip").forEach((button) => {
    button.addEventListener("click", () => {
      state[side].excluded = state[side].excluded.filter(
        (place) => place.code !== button.dataset.code
      );
      renderExcludeChips(side);
    });
  });
}

function bindExcludeAutocomplete(inputId) {
  const input = $(inputId);
  let timer = null;
  input.addEventListener("input", () => {
    clearTimeout(timer);
    const query = input.value.trim();
    if (query.length < 2) return;
    timer = setTimeout(async () => {
      try {
        const response = await fetch(`/api/places?q=${encodeURIComponent(query)}&limit=20`);
        if (!response.ok) return;
        const places = await response.json();
        $("exclude-list").innerHTML = places
          .map(
            (place) =>
              `<option value="${esc(place.code)}">${esc(place.code)} — ${esc(place.name)}${
                place.kind === "country" ? " (страна)" : ""
              }</option>`
          )
          .join("");
      } catch (error) {
        return;
      }
    }, 220);
  });
}

async function addExcludedFromInput(side) {
  const input = $(`${side}-exclude`);
  const parts = input.value
    .split(/[,\n]+/)
    .map((value) => value.trim())
    .filter(Boolean);
  if (parts.length === 0) return;
  const errors = [];
  for (const part of parts) {
    try {
      const response = await fetch(`/api/places?q=${encodeURIComponent(part)}&limit=20`);
      if (!response.ok) {
        errors.push(part);
        continue;
      }
      const items = await response.json();
      let place = items.find((item) => item.code.toUpperCase() === part.toUpperCase());
      if (!place) {
        place = items.find((item) => item.name.toLowerCase() === part.toLowerCase());
      }
      if (!place) place = items[0];
      if (!place) {
        errors.push(part);
        continue;
      }
      if (
        !state[side].excluded.some(
          (item) => item.code === place.code && item.kind === place.kind
        )
      ) {
        state[side].excluded.push({
          kind: place.kind,
          code: place.code,
          name: place.name,
        });
      }
    } catch (error) {
      errors.push(part);
    }
  }
  input.value = "";
  renderExcludeChips(side);
  if (errors.length > 0) {
    setStatus(`Не найдено: ${errors.join(", ")}`, "error");
  }
}

function endpointPayload(which) {
  const item = state[which];
  if (item.mode === "any") {
    return { mode: "any" };
  }
  if (item.mode === "place") {
    const raw = $(`${which}-country`).value.trim();
    const values = [
      ...item.places.map((place) => place.code),
      ...raw
        .split(/[,\n]+/)
        .map((value) => value.trim())
        .filter(Boolean),
    ];
    if (values.length === 0) {
      throw new Error(
        which === "origin"
          ? "Укажите город или страну отправления."
          : "Укажите город или страну назначения."
      );
    }
    return { mode: "auto", codes: values };
  }
  if (item.mode === "region") {
    if (!item.point) {
      throw new Error(
        which === "origin"
          ? "Укажите точку отправления на карте."
          : "Укажите точку назначения на карте."
      );
    }
    return {
      mode: "region",
      lat: item.point.lat,
      lon: item.point.lon,
      radius_km: item.radius,
      limit: 100,
    };
  }
  const code = $(`${which}-city`).value.trim();
  if (!code) {
    throw new Error(which === "origin" ? "Укажите город отправления." : "Укажите город назначения.");
  }
  return { mode: "city", code };
}

function splitList(value) {
  return value
    .split(/[,\n]+/)
    .map((item) => item.trim())
    .filter(Boolean);
}

function buildPayload() {
  const payload = {
    origin: endpointPayload("origin"),
    destination: endpointPayload("destination"),
    max_transfers: numberValue("max-transfers"),
    max_connection_days: numberValue("max-connection-days"),
    exclude_origin_places: [
      ...state.origin.excluded.map((place) => place.code),
      ...splitList($("origin-exclude").value),
    ],
    exclude_destination_places: [
      ...state.destination.excluded.map((place) => place.code),
      ...splitList($("destination-exclude").value),
    ],
  };
  const dates = {
    depart_from: $("depart-from").value,
    depart_to: $("depart-to").value,
    arrive_from: $("arrive-from").value,
    arrive_to: $("arrive-to").value,
  };
  for (const [key, value] of Object.entries(dates)) {
    if (value) payload[key] = value;
  }
  return payload;
}

function updateCacheInfo(cache) {
  if (!cache) {
    $("cache-info").textContent = "Кэш: —";
    return;
  }
  $("cache-info").textContent = `Кэш: ${cache.routes} маршрутов · ${cache.offers} предложений`;
}

async function refreshCacheInfo() {
  try {
    const response = await fetch("/api/cache");
    if (!response.ok) return;
    updateCacheInfo(await response.json());
  } catch (error) {
    return;
  }
}

function numberHuman(value) {
  return new Intl.NumberFormat("ru-RU").format(Number(value) || 0);
}

function formatTimestamp(value) {
  if (!value) return "—";
  return String(value).replace("T", " ").replace("Z", "").slice(0, 16);
}

function updateDbInfo(db) {
  if (!db) {
    $("db-info").textContent = "БД/RAM: —";
    return;
  }
  const updated = formatTimestamp(db.last_bulk_finished || db.last_bulk_started);
  $("db-info").textContent = `БД: ${numberHuman(db.offers)} билетов · ${numberHuman(
    db.routes
  )} маршрутов · RAM: ${numberHuman(db.ram_offers)} билетов · обновлено ${updated}`;
}

async function refreshDbInfo() {
  try {
    const response = await fetch("/api/db");
    if (!response.ok) return;
    updateDbInfo(await response.json());
  } catch (error) {
    return;
  }
}

function renderWarnings(warnings) {
  const target = $("warnings");
  if (!Array.isArray(warnings) || warnings.length === 0) {
    target.innerHTML = "";
    return;
  }
  target.innerHTML = warnings
    .map((warning) => `<div class="warning-line">${esc(warning)}</div>`)
    .join("");
}

function drawSearchCities(data) {
  clearLayer(state.searchLayer);
  const origins = Array.isArray(data.origin_cities) ? data.origin_cities.slice(0, 30) : [];
  const destinations = Array.isArray(data.destination_cities)
    ? data.destination_cities.slice(0, 30)
    : [];
  if (state.origin.mode !== "any") {
    origins.forEach((city) => {
      L.circleMarker([city.lat, city.lon], {
        radius: 4,
        color: "#fff",
        weight: 1,
        fillColor: "#2563eb",
        fillOpacity: 0.85,
      })
        .bindTooltip(`Откуда: ${city.code} · ${city.name}`)
        .addTo(state.searchLayer);
    });
  }
  if (state.destination.mode !== "any") {
    destinations.forEach((city) => {
      L.circleMarker([city.lat, city.lon], {
        radius: 4,
        color: "#fff",
        weight: 1,
        fillColor: "#dc2626",
        fillOpacity: 0.85,
      })
        .bindTooltip(`Куда: ${city.code} · ${city.name}`)
        .addTo(state.searchLayer);
    });
  }
}

function drawItinerary(index) {
  const itinerary = state.itineraries[index];
  if (!itinerary) return;
  clearLayer(state.routeLayer);
  const points = [];
  itinerary.legs.forEach((leg) => {
    if (
      !Number.isFinite(leg.origin_lat) ||
      !Number.isFinite(leg.origin_lon) ||
      !Number.isFinite(leg.destination_lat) ||
      !Number.isFinite(leg.destination_lon)
    ) {
      return;
    }
    const from = [leg.origin_lat, leg.origin_lon];
    const to = [leg.destination_lat, leg.destination_lon];
    points.push(from, to);
    L.polyline([from, to], {
      color: "#2563eb",
      weight: 4,
      opacity: 0.9,
      lineJoin: "round",
    }).addTo(state.routeLayer);
    L.circleMarker(from, {
      radius: 5,
      color: "#fff",
      weight: 2,
      fillColor: "#1d4ed8",
      fillOpacity: 1,
    })
      .bindPopup(`${esc(leg.origin_airport)} · ${esc(leg.origin_name)}`)
      .addTo(state.routeLayer);
    L.circleMarker(to, {
      radius: 5,
      color: "#fff",
      weight: 2,
      fillColor: "#1d4ed8",
      fillOpacity: 1,
    })
      .bindPopup(`${esc(leg.destination_airport)} · ${esc(leg.destination_name)}`)
      .addTo(state.routeLayer);
  });
  if (points.length > 0) {
    state.map.fitBounds(L.latLngBounds(points), { padding: [40, 40], maxZoom: 7 });
  }
  state.activeIndex = index;
  document.querySelectorAll(".itinerary").forEach((card) => {
    card.classList.toggle("active", Number(card.dataset.index) === index);
  });
}

function legHtml(leg, index, total, nextDepart, currency) {
  const depart = dateParts(leg.depart_at);
  const arrive = dateParts(leg.arrive_at);
  const link = safeUrl(leg.link);
  const linkHtml = link
    ? `<a class="buy-link" href="${esc(link)}" target="_blank" rel="noopener noreferrer">Открыть билет</a>`
    : "";
  const transferHtml =
    index < total - 1 && nextDepart
      ? `<div class="transfer-note">Стыковка в ${esc(leg.destination_name)}: ${minutesHuman(
          (new Date(nextDepart) - new Date(leg.arrive_at)) / 60000
        )}</div>`
      : "";
  return {
    leg: `
      <div class="leg">
        <div class="leg-time">${esc(depart.date)}<br>${esc(depart.time)}</div>
        <div class="leg-route">
          <strong>${esc(leg.origin_airport)} → ${esc(leg.destination_airport)}</strong>
          <div class="muted">${esc(leg.origin_name)} → ${esc(leg.destination_name)} · ${esc(
            leg.airline || "—"
          )} ${esc(leg.flight_number || "")}${leg.transfers ? ` · ${transfersText(leg.transfers)} внутри билета` : ""}</div>
          <div class="muted">прилёт ${esc(arrive.date)}, ${esc(arrive.time)}</div>
        </div>
        <div class="leg-meta">
          ${minutesHuman(leg.duration_min)}<br>
          ${priceHuman(leg.price)} ${esc(currency.toUpperCase())}<br>
          ${linkHtml}
        </div>
      </div>
    `,
    transfer: transferHtml,
  };
}

function itineraryHtml(itinerary, index) {
  const currency = itinerary.currency || "rub";
  const depart = dateParts(itinerary.depart_at);
  const arrive = dateParts(itinerary.arrive_at);
  const legs = itinerary.legs || [];
  const parts = [];
  legs.forEach((leg, legIndex) => {
    const nextDepart = legIndex < legs.length - 1 ? legs[legIndex + 1].depart_at : null;
    const result = legHtml(leg, legIndex, legs.length, nextDepart, currency);
    parts.push(result.leg);
    if (result.transfer) parts.push(result.transfer);
  });
  const firstLink = safeUrl(legs[0]?.link);
  const ticketLink = firstLink
    ? `<a class="buy-link primary-ticket-link" href="${esc(firstLink)}" target="_blank" rel="noopener noreferrer">${
        legs.length === 1 ? "Открыть билет на Aviasales" : "Открыть первый билет"
      }</a>`
    : "";
  const ticketNote =
    legs.length > 1
      ? `<div class="ticket-note">Составной маршрут: каждый сегмент — отдельный билет.</div>`
      : "";
  return `
    <article class="itinerary" data-index="${index}">
      <div class="itinerary-top">
        <div>
          <div class="price">${priceHuman(itinerary.total_price)} <small>${esc(
            (itinerary.currency || "").toUpperCase()
          )}</small></div>
          ${ticketLink}
          ${ticketNote}
        </div>
        <div class="summary">
          <span class="pill blue">${esc(transfersText(itinerary.total_transfers))}</span>
          <span class="pill">в пути ${minutesHuman(itinerary.travel_minutes)}</span>
          <span class="pill">вылет ${esc(depart.date)}, ${esc(depart.time)}</span>
          <span class="pill">прилёт ${esc(arrive.date)}, ${esc(arrive.time)}</span>
        </div>
      </div>
      <div class="times">${esc(legs[0]?.origin_airport || "—")} → ${esc(
        legs[legs.length - 1]?.destination_airport || "—"
      )}<span class="arrow">·</span>${esc(depart.date)}, ${esc(depart.time)} → ${esc(
        arrive.date
      )}, ${esc(arrive.time)}</div>
      <div class="legs">${parts.join("")}</div>
    </article>
  `;
}

function itineraryDirection(itinerary) {
  const legs = itinerary.legs || [];
  const first = legs[0];
  const last = legs[legs.length - 1];
  return {
    key: `${first?.origin || ""}|${last?.destination || ""}`,
    originCode: first?.origin || "—",
    originName: first?.origin_name || first?.origin || "—",
    destCode: last?.destination || "—",
    destName: last?.destination_name || last?.destination || "—",
  };
}

function renderGroup(group) {
  state.activeGroup = group;
  const order =
    state.groups[group] && state.groups[group].length
      ? state.groups[group]
      : state.itineraries.map((_, index) => index);
  const rank = new Map(order.map((index, position) => [index, position]));
  const byDirection = new Map();
  state.itineraries.forEach((itinerary, index) => {
    const direction = itineraryDirection(itinerary);
    if (!byDirection.has(direction.key)) {
      byDirection.set(direction.key, { ...direction, variants: [] });
    }
    byDirection.get(direction.key).variants.push(index);
  });
  const directions = [...byDirection.values()];
  directions.forEach((direction) => {
    direction.variants.sort(
      (a, b) => (rank.has(a) ? rank.get(a) : 1e9) - (rank.has(b) ? rank.get(b) : 1e9)
    );
    direction.best = direction.variants[0];
  });
  directions.sort(
    (a, b) =>
      (rank.has(a.best) ? rank.get(a.best) : 1e9) -
      (rank.has(b.best) ? rank.get(b.best) : 1e9)
  );

  const list = $("results-list");
  list.innerHTML = directions
    .map((direction) => {
      const best = state.itineraries[direction.best];
      const dates = new Map();
      direction.variants.forEach((index) => {
        const itinerary = state.itineraries[index];
        const key = (itinerary.depart_at || "").slice(0, 10);
        if (!dates.has(key)) dates.set(key, []);
        dates.get(key).push(index);
      });
      const dateKeys = [...dates.keys()];
      const toggleLabel =
        dateKeys.length > 1
          ? `Есть другие даты (${dateKeys.length})`
          : `Все варианты (${direction.variants.length})`;
      const head = `
        <div class="destination-head">
          <span class="destination-code">${esc(direction.originCode)} → ${esc(
            direction.destCode
          )} · ${esc(direction.originName)} → ${esc(direction.destName)}</span>
          ${
            direction.variants.length > 1
              ? `<span class="details-toggle">${esc(toggleLabel)}</span>`
              : ""
          }
        </div>
      `;
      if (direction.variants.length <= 1) {
        return `<div class="destination-group" data-direction="${esc(
          direction.key
        )}">${head}${itineraryHtml(best, direction.best)}</div>`;
      }
      const variantBlocks = [...dates.entries()]
        .map(([dateKey, indexes]) => {
          const sample = state.itineraries[indexes[0]];
          const label = dateParts(sample.depart_at).date;
          return `
            <div class="variant-date">
              <div class="variant-date-head">${esc(label)} · ${indexes.length} ${plural(
                indexes.length,
                ["вариант", "варианта", "вариантов"]
              )}</div>
              ${indexes
                .map(
                  (index) =>
                    `<div class="variant-wrap">${itineraryHtml(
                      state.itineraries[index],
                      index
                    )}</div>`
                )
                .join("")}
            </div>
          `;
        })
        .join("");
      return `
        <details class="destination-group" data-direction="${esc(direction.key)}">
          <summary>${head}${itineraryHtml(best, direction.best)}</summary>
          <div class="variants-list">${variantBlocks}</div>
        </details>
      `;
    })
    .join("");

  document.querySelectorAll(".variants-list .itinerary").forEach((card) => {
    card.addEventListener("click", (event) => {
      if (event.target.closest("a")) return;
      drawItinerary(Number(card.dataset.index));
    });
  });
  document.querySelectorAll(".destination-group > summary").forEach((summary) => {
    summary.addEventListener("click", (event) => {
      if (event.target.closest("a")) return;
      const card = summary.querySelector(".itinerary");
      if (card) drawItinerary(Number(card.dataset.index));
    });
  });
  document.querySelectorAll(".tab").forEach((tab) => {
    tab.classList.toggle("active", tab.dataset.group === group);
  });
  $("results-count").textContent = `Направлений: ${directions.length} · вариантов: ${
    state.itineraries.length
  }`;
  if (directions.length > 0) {
    drawItinerary(directions[0].best);
  } else {
    clearLayer(state.routeLayer);
  }
}

function renderResults(data) {
  state.itineraries = Array.isArray(data.itineraries) ? data.itineraries : [];
  state.groups = data.groups || {
    price: state.itineraries.map((_, index) => index),
    transfers: state.itineraries.map((_, index) => index),
    dates: state.itineraries.map((_, index) => index),
    duration: state.itineraries.map((_, index) => index),
  };
  state.activeGroup = "price";
  state.activeIndex = -1;
  const count = state.itineraries.length;
  $("results-count").textContent = count ? `Найдено: ${count}` : "Ничего не найдено";
  const source = data.source === "ram" ? "RAM" : "БД/API";
  const failed = data.routes_failed ? ` · ошибок ${data.routes_failed}` : "";
  $("search-info").textContent = `Источник: ${source} · маршрутов ${numberHuman(
    data.routes_requested
  )} · предложений ${numberHuman(data.offers_considered)}${failed} · время ${(
    data.elapsed_ms / 1000
  ).toFixed(1)} с`;
  updateCacheInfo(data.cache);
  renderWarnings(data.warnings);
  drawSearchCities(data);
  refreshDbInfo();

  const list = $("results-list");
  $("results-tabs").classList.toggle("hidden", count === 0);
  if (count === 0) {
    const warnings = Array.isArray(data.warnings) ? data.warnings.slice(0, 3).join(" ") : "";
    list.innerHTML = `<div class="empty-state">${esc(
      warnings || "Маршруты не найдены. Попробуйте изменить даты или ограничения."
    )}</div>`;
    clearLayer(state.routeLayer);
    return;
  }

  const preferred = Object.keys(state.groups).find(
    (group) => state.groups[group] && state.groups[group].length > 0
  );
  renderGroup(preferred || "price");
}

async function runSearch() {
  const button = $("search-button");
  button.disabled = true;
  setStatus("Ищу маршруты: загружаю цены и перебираю связки...", "");
  try {
    const payload = buildPayload();
    const response = await fetch("/api/search", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
    let data = null;
    try {
      data = await response.json();
    } catch (error) {
      data = null;
    }
    if (!response.ok) {
      throw new Error((data && data.error) || `Ошибка HTTP ${response.status}`);
    }
    renderResults(data);
    setStatus(`Готово за ${(data.elapsed_ms / 1000).toFixed(1)} с`, "ok");
  } catch (error) {
    setStatus(error.message || String(error), "error");
  } finally {
    button.disabled = false;
  }
}

function initMap() {
  state.map = L.map("map", { zoomControl: true }).setView([52, 25], 3);
  L.tileLayer("https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png", {
    maxZoom: 18,
    attribution: "&copy; OpenStreetMap",
  }).addTo(state.map);
  state.originPreviewLayer = L.layerGroup().addTo(state.map);
  state.destinationPreviewLayer = L.layerGroup().addTo(state.map);
  state.searchLayer = L.layerGroup().addTo(state.map);
  state.routeLayer = L.layerGroup().addTo(state.map);
  state.map.on("click", (event) => {
    if (!state.pickMode) return;
    const which = state.pickMode;
    $(`${which}-mode`).value = "region";
    state[which].mode = "region";
    updateModeUI(which);
    setRegionPoint(which, event.latlng.lat, event.latlng.lng);
    state.pickMode = null;
    updatePickButtons();
    setStatus(
      which === "origin" ? "Точка отправления выбрана." : "Точка назначения выбрана.",
      "ok"
    );
  });
}

function init() {
  initMap();

  ["origin", "destination"].forEach((which) => {
    $(`${which}-mode`).addEventListener("change", () => updateModeUI(which));
    $(`${which}-radius`).addEventListener("input", (event) => syncRadius(which, event.target.value));
    $(`${which}-radius-num`).addEventListener("input", (event) =>
      syncRadius(which, event.target.value)
    );
    state[which].radius = Number($(`${which}-radius`).value) || 150;
    updateModeUI(which);
  });

  document.querySelectorAll(".pick-button").forEach((button) => {
    button.addEventListener("click", () => {
      const which = button.dataset.pick;
      state.pickMode = state.pickMode === which ? null : which;
      updatePickButtons();
      setStatus(
        state.pickMode
          ? `Кликните на карте, чтобы задать точку ${which === "origin" ? "отправления" : "назначения"}.`
          : "",
        ""
      );
    });
  });

  bindPlaceAutocomplete("origin-country");
  bindPlaceAutocomplete("destination-country");
  bindExcludeAutocomplete("origin-exclude");
  bindExcludeAutocomplete("destination-exclude");
  document.querySelectorAll(".add-country").forEach((button) => {
    button.addEventListener("click", () => addPlacesFromInput(button.dataset.side));
  });
  document.querySelectorAll(".add-exclude").forEach((button) => {
    button.addEventListener("click", () => addExcludedFromInput(button.dataset.side));
  });
  ["origin", "destination"].forEach((side) => {
    $(`${side}-country`).addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        addPlacesFromInput(side);
      }
    });
    $(`${side}-exclude`).addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        addExcludedFromInput(side);
      }
    });
  });
  $("search-button").addEventListener("click", runSearch);
  $("results-tabs").addEventListener("click", (event) => {
    const tab = event.target.closest(".tab");
    if (!tab) return;
    renderGroup(tab.dataset.group);
  });
  refreshCacheInfo();
  refreshDbInfo();
}

document.addEventListener("DOMContentLoaded", init);
