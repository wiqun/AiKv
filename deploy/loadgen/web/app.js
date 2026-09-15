const FIELDS = ["target_ops", "connections", "pipeline", "timeout_ms", "keyspace", "key_prefix",
    "value_size_min", "value_size_max", "miss_ratio", "ttl_ratio", "ttl_seconds"];
let ALL_OPS = [];
let PRESETS = [];
let SUPPORTED_COMMANDS = new Set(["GET", "SET", "MGET", "DEL", "INCR", "EXPIRE"]);
let COMMAND_MIX_KEYS = { GET: "get", SET: "set", MGET: "mget", DEL: "del", INCR: "incr", EXPIRE: "expire" };
const HEAVY_HINTS = {};

function mixKeyOf(cmdUpper) {
    return COMMAND_MIX_KEYS[cmdUpper] || cmdUpper.toLowerCase().replace(/\./g, "_");
}

async function loadEngineCatalog() {
    const res = await fetch("/api/commands");
    const json = await res.json();
    const list = json.commands || [];
    SUPPORTED_COMMANDS = new Set(list.map(c => c.name));
    COMMAND_MIX_KEYS = Object.fromEntries(list.map(c => [c.name, c.mix_key]));
    for (const key of Object.keys(HEAVY_HINTS)) delete HEAVY_HINTS[key];
    for (const c of list) {
        if (c.heavy_hint) HEAVY_HINTS[c.mix_key] = c.heavy_hint;
        if (!OP_META[c.mix_key]) {
            OP_META[c.mix_key] = {
                name: c.name,
                color: COLOR_PALETTE[Object.keys(OP_META).length % COLOR_PALETTE.length],
                desc: `${c.group} 命令 ${c.name}`
            };
        } else {
            OP_META[c.mix_key].name = c.name;
        }
    }
}
const OP_META = {
    get: { name: "GET", color: "#a855f7", desc: "读取数据" },
    set: { name: "SET", color: "#22c55e", desc: "写入数据" },
    mget: { name: "MGET", color: "#f97316", desc: "批量读取" },
    del: { name: "DEL", color: "#3b82f6", desc: "删除数据" },
    incr: { name: "INCR", color: "#ef4444", desc: "数值自增" },
    expire: { name: "EXPIRE", color: "#eab308", desc: "设置过期" }
};
const COLOR_PALETTE = [
    "#ec4899", "#14b8a6", "#06b6d4", "#f43f5e", "#84cc16",
    "#0ea5e9", "#d946ef", "#e11d48", "#10b981", "#6366f1",
    "#f59e0b", "#8b5cf6", "#3b82f6", "#22c55e", "#a855f7"
];
let activeSuggestIndex = -1;
let currentSuggestions = [];
let currentPresetKey = null;
const NUMERIC = new Set(["target_ops", "connections", "pipeline", "timeout_ms", "keyspace",
    "value_size_min", "value_size_max", "miss_ratio", "ttl_ratio", "ttl_seconds"]);

const CRC16_TABLE = [
    0x0000, 0x1021, 0x2042, 0x3063, 0x4084, 0x50a5, 0x60c6, 0x70e7, 0x8108, 0x9129, 0xa14a, 0xb16b,
    0xc18c, 0xd1ad, 0xe1ce, 0xf1ef, 0x1231, 0x0210, 0x3273, 0x2252, 0x52b5, 0x4294, 0x72f7, 0x62d6,
    0x9339, 0x8318, 0xb37b, 0xa35a, 0xd3bd, 0xc39c, 0xf3ff, 0xe3de, 0x2462, 0x3443, 0x0420, 0x1401,
    0x64e6, 0x74c7, 0x44a4, 0x5485, 0xa56a, 0xb54b, 0x8528, 0x9509, 0xe5ee, 0xf5cf, 0xc5ac, 0xd58d,
    0x3653, 0x2672, 0x1611, 0x0630, 0x76d7, 0x66f6, 0x5695, 0x46b4, 0xb75b, 0xa77a, 0x9719, 0x8738,
    0xf7df, 0xe7fe, 0xd79d, 0xc7bc, 0x48c4, 0x58e5, 0x6886, 0x78a7, 0x0840, 0x1861, 0x2802, 0x3823,
    0xc9cc, 0xd9ed, 0xe98e, 0xf9af, 0x8948, 0x9969, 0xa90a, 0xb92b, 0x5af5, 0x4ad4, 0x7ab7, 0x6a96,
    0x1a71, 0x0a50, 0x3a33, 0x2a12, 0xdbfd, 0xcbdc, 0xfbbf, 0xeb9e, 0x9b79, 0x8b58, 0xbb3b, 0xab1a,
    0x6ca6, 0x7c87, 0x4ce4, 0x5cc5, 0x2c22, 0x3c03, 0x0c60, 0x1c41, 0xedae, 0xfd8f, 0xcdec, 0xddcd,
    0xad2a, 0xbd0b, 0x8d68, 0x9d49, 0x7e97, 0x6eb6, 0x5ed5, 0x4ef4, 0x3e13, 0x2e32, 0x1e51, 0x0e70,
    0xff9f, 0xefbe, 0xdfdd, 0xcffc, 0xbf1b, 0xaf3a, 0x9f59, 0x8f78, 0x9188, 0x81a9, 0xb1ca, 0xa1eb,
    0xd10c, 0xc12d, 0xf14e, 0xe16f, 0x1080, 0x00a1, 0x30c2, 0x20e3, 0x5004, 0x4025, 0x7046, 0x6067,
    0x83b9, 0x9398, 0xa3fb, 0xb3da, 0xc33d, 0xd31c, 0xe37f, 0xf35e, 0x02b1, 0x1290, 0x22f3, 0x32d2,
    0x4235, 0x5214, 0x6277, 0x7256, 0xb5ea, 0xa5cb, 0x95a8, 0x8589, 0xf56e, 0xe54f, 0xd52c, 0xc50d,
    0x34e2, 0x24c3, 0x14a0, 0x0481, 0x7466, 0x6447, 0x5424, 0x4405, 0xa7db, 0xb7fa, 0x8799, 0x97b8,
    0xe75f, 0xf77e, 0xc71d, 0xd73c, 0x26d3, 0x36f2, 0x0691, 0x16b0, 0x6657, 0x7676, 0x4615, 0x5634,
    0xd94c, 0xc96d, 0xf90e, 0xe92f, 0x99c8, 0x89e9, 0xb98a, 0xa9ab, 0x5844, 0x4865, 0x7806, 0x6827,
    0x18c0, 0x08e1, 0x3882, 0x28a3, 0xcb7d, 0xdb5c, 0xeb3f, 0xfb1e, 0x8bf9, 0x9bd8, 0xabbb, 0xbb9a,
    0x4a75, 0x5a54, 0x6a37, 0x7a16, 0x0af1, 0x1ad0, 0x2ab3, 0x3a92, 0xfd2e, 0xed0f, 0xdd6c, 0xcd4d,
    0xbdaa, 0xad8b, 0x9de8, 0x8dc9, 0x7c26, 0x6c07, 0x5c64, 0x4c45, 0x3ca2, 0x2c83, 0x1ce0, 0x0cc1,
    0xef1f, 0xff3e, 0xcf5d, 0xdf7c, 0xaf9b, 0xbfba, 0x8fd9, 0x9ff8, 0x6e17, 0x7e36, 0x4e55, 0x5e74,
    0x2e93, 0x3eb2, 0x0ed1, 0x1ef0
];

function calcPrefixSlot(prefix) {
    const p = prefix || "loadgen";
    let crc = 0;
    for (let i = 0; i < p.length; i++) {
        const b = p.charCodeAt(i) & 0xff;
        const idx = ((crc >> 8) ^ b) & 0xff;
        crc = ((crc << 8) & 0xffff) ^ CRC16_TABLE[idx];
    }
    return crc % 16384;
}

function updateSlotPreview() {
    const preview = el("slot_preview");
    if (!preview) return;
    const input = el("target_slot");
    const raw = input ? input.value.trim() : "";
    const isApple = preview.classList.contains("apple-slot-badge") || preview.className.includes("apple");
    const baseClass = isApple ? "apple-slot-badge" : "slot-status-badge";
    if (raw === "") {
        preview.textContent = "全槽分散";
        preview.className = baseClass;
        return;
    }
    const s = Number(raw);
    if (!isNaN(s) && Number.isInteger(s) && s >= 0 && s <= 16383) {
        preview.textContent = `Slot ${s}`;
        preview.className = `${baseClass} active`;
    } else {
        preview.textContent = "非法 0-16383";
        preview.className = `${baseClass} error`;
    }
}

let dirty = false;
let lastToast = "";
let toastTimer = null;

let activeOps = new Set();
let opPercentages = {};
let unallocatedPct = 0;
let draggingOp = null;

function el(id) { return document.getElementById(id); }

function syncHitRange(val) {
    if (el("hit_ratio_display")) el("hit_ratio_display").textContent = Math.round(Number(val) * 100) + "%";
    markDirty();
}
function syncTtlRange(val) {
    if (el("ttl_ratio_display")) el("ttl_ratio_display").textContent = Math.round(Number(val) * 100) + "%";
    markDirty();
}

function splitEndpoint(addr) {
    const value = (addr || "127.0.0.1:6379").trim();
    const cut = value.lastIndexOf(":");
    if (cut <= 0) return { host: value || "127.0.0.1", port: "6379" };
    return { host: value.slice(0, cut), port: value.slice(cut + 1) };
}


function getActiveList() {
    return ALL_OPS.filter(op => activeOps.has(op));
}

function calcUnallocated() {
    const sum = getActiveList().reduce((acc, op) => acc + (opPercentages[op] || 0), 0);
    unallocatedPct = Math.max(0, 100 - sum);
    return unallocatedPct;
}

function renderCommandPills() {
    const box = el("cmd_pills");
    if (!box) return;
    box.innerHTML = ALL_OPS.map(op => {
        const isActive = activeOps.has(op);
        const meta = OP_META[op] || { name: op.toUpperCase(), color: "#94a3b8", desc: "" };
        return `
      <div class="cmd-pill ${isActive ? 'active' : ''}" 
   style="--pill-color: ${meta.color}" 
   onclick="toggleOp('${op}')" 
   title="${meta.desc || meta.name}">
<span class="pill-dot"></span>
<span>${meta.name}</span>
<span class="pill-check">✓</span>
      </div>
    `;
    }).join("");
}

let isAddingCmd = false;

function startAddCmd(e) {
    if (e) e.stopPropagation();
    isAddingCmd = true;
    const container = el("cmd_add_container");
    if (!container) return;
    container.classList.add("is-open");
    container.innerHTML = `
        <div class="cmd-add-combobox">
            <div class="cmd-pill add-pill input-mode" id="cmd_add_pill" onclick="event.stopPropagation()">
                <input type="text" id="cmd_add_input" class="cmd-add-input"
                       placeholder="输入命令" maxlength="20" size="8" autocomplete="off" spellcheck="false" />
                <span class="cmd-add-btn-check" id="cmd_add_submit_btn" onclick="submitAddCmd()" title="确认添加">
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"></polyline></svg>
                </span>
            </div>
            <div class="cmd-suggestions-menu" id="cmd_suggestions_menu"></div>
        </div>
    `;
    initCmdAddInput();
    const input = el("cmd_add_input");
    if (input) {
        input.focus();
        renderCmdSuggestions();
    }
}

function resetAddPill() {
    isAddingCmd = false;
    hideCmdSuggestions();
    const container = el("cmd_add_container");
    if (!container) return;
    container.classList.remove("is-open");
    container.innerHTML = `
        <div class="cmd-pill add-pill" id="cmd_add_pill" onclick="startAddCmd(event)" title="添加命令">
            <span>+ 添加命令</span>
        </div>
    `;
}

function initCmdAddInput() {
    const input = el("cmd_add_input");
    if (!input) return;

    input.addEventListener("input", () => {
        renderCmdSuggestions();
    });

    input.addEventListener("focus", () => {
        renderCmdSuggestions();
    });

    input.addEventListener("keydown", (e) => {
        const menu = el("cmd_suggestions_menu");
        const isOpen = menu && menu.classList.contains("open");

        if (e.key === "ArrowDown") {
            if (isOpen && currentSuggestions.length > 0) {
                e.preventDefault();
                activeSuggestIndex = (activeSuggestIndex + 1) % currentSuggestions.length;
                updateSuggestHighlight();
            }
        } else if (e.key === "ArrowUp") {
            if (isOpen && currentSuggestions.length > 0) {
                e.preventDefault();
                activeSuggestIndex = (activeSuggestIndex - 1 + currentSuggestions.length) % currentSuggestions.length;
                updateSuggestHighlight();
            }
        } else if (e.key === "Enter") {
            e.preventDefault();
            if (isOpen && activeSuggestIndex >= 0 && activeSuggestIndex < currentSuggestions.length) {
                submitAddCmd(currentSuggestions[activeSuggestIndex]);
            } else {
                submitAddCmd();
            }
        } else if (e.key === "Escape") {
            e.preventDefault();
            resetAddPill();
        }
    });
}

// 全局点击监听：外部点击时处理菜单隐藏与空输入收起
document.addEventListener("click", (e) => {
    const container = el("cmd_add_container");
    if (container && !container.contains(e.target)) {
        hideCmdSuggestions();
        if (isAddingCmd) {
            const input = el("cmd_add_input");
            // 仅当输入框为空时恢复为默认胶囊，保留正在输入的内容避免误关
            if (!input || !input.value.trim()) {
                resetAddPill();
            }
        }
    }
});

function renderCmdSuggestions() {
    const input = el("cmd_add_input");
    const menu = el("cmd_suggestions_menu");
    if (!input || !menu) return;

    const q = (input.value || "").trim().toUpperCase();
    const allCmds = Array.from(SUPPORTED_COMMANDS).sort();

    if (!q) {
        const popular = ["MSET", "HGET", "HSET", "LPUSH", "RPOP", "ZADD", "SADD", "JSON.GET", "JSON.SET", "PING"];
        currentSuggestions = popular.filter(cmd => SUPPORTED_COMMANDS.has(cmd));
    } else {
        const starts = allCmds.filter(cmd => cmd.startsWith(q));
        const includes = allCmds.filter(cmd => !cmd.startsWith(q) && cmd.includes(q));
        currentSuggestions = starts.concat(includes).slice(0, 8);
    }

    activeSuggestIndex = -1;

    if (currentSuggestions.length === 0) {
        menu.innerHTML = `<div class="cmd-suggestions-empty">无匹配可选命令</div>`;
        menu.classList.add("open");
        return;
    }

    menu.innerHTML = currentSuggestions.map((cmd, idx) => {
        const opKey = mixKeyOf(cmd);
        const isAdded = activeOps.has(opKey);
        return `
            <div class="cmd-suggest-item ${isAdded ? 'added' : ''}"
                 data-index="${idx}"
                 onmousedown="event.preventDefault(); submitAddCmd('${cmd}');">
                <span>${cmd}</span>
                ${isAdded ? '<span class="cmd-suggest-tag">已添加</span>' : ''}
            </div>
        `;
    }).join("");
    menu.classList.add("open");
}

function updateSuggestHighlight() {
    const items = document.querySelectorAll(".cmd-suggest-item");
    items.forEach((item, idx) => {
        if (idx === activeSuggestIndex) {
            item.classList.add("highlighted");
            item.scrollIntoView({ block: "nearest" });
        } else {
            item.classList.remove("highlighted");
        }
    });
}

function hideCmdSuggestions() {
    const menu = el("cmd_suggestions_menu");
    if (menu) menu.classList.remove("open");
    activeSuggestIndex = -1;
}

function submitAddCmd(specificCmd) {
    const input = el("cmd_add_input");
    const val = (specificCmd || (input ? input.value : "") || "").trim();
    if (!val) {
        resetAddPill();
        return;
    }
    const cmdUpper = val.toUpperCase();
    if (!SUPPORTED_COMMANDS.has(cmdUpper)) {
        showToast(`引擎不支持发压命令: ${cmdUpper}`);
        if (input) {
            input.value = "";
            input.focus();
        }
        hideCmdSuggestions();
        return;
    }

    const opKey = mixKeyOf(cmdUpper);
    if (!OP_META[opKey]) {
        const color = COLOR_PALETTE[ALL_OPS.length % COLOR_PALETTE.length];
        OP_META[opKey] = {
            name: cmdUpper,
            color: color,
            desc: `客户端命令 ${cmdUpper}`
        };
    }
    if (!ALL_OPS.includes(opKey)) {
        ALL_OPS.push(opKey);
    }

    if (!activeOps.has(opKey)) {
        activeOps.add(opKey);
        calcUnallocated();
        if (unallocatedPct > 0) {
            const give = Math.min(10, unallocatedPct);
            opPercentages[opKey] = give;
        } else {
            opPercentages[opKey] = 0;
        }
        showToast(`成功添加命令: ${cmdUpper}`);
        warnIfHeavy(opKey);
    } else {
        showToast(`命令 ${cmdUpper} 已在已选测试列表中`);
    }

    resetAddPill();
    syncMixUI();
    markDirty();
}

function toggleOp(op) {
    if (activeOps.has(op)) {
        if (activeOps.size <= 1) {
            showToast("至少需要保留一种测试命令");
            return;
        }
        activeOps.delete(op);
        opPercentages[op] = 0;
    } else {
        activeOps.add(op);
        calcUnallocated();
        if (unallocatedPct > 0) {
            const give = Math.min(10, unallocatedPct);
            opPercentages[op] = give;
        } else {
            opPercentages[op] = 0;
        }
        warnIfHeavy(op);
    }
    syncMixUI();
    markDirty();
}

function updateOpPct(op, newVal) {
    calcUnallocated();
    const oldVal = opPercentages[op] || 0;
    let target = Math.max(0, Math.min(100, Math.round(Number(newVal) || 0)));
    const diff = target - oldVal;
    if (diff > 0) {
        const allowed = Math.min(diff, unallocatedPct);
        target = oldVal + allowed;
    }
    opPercentages[op] = target;
    syncMixUI();
    markDirty();
}

function splitEvenly() {
    const list = getActiveList();
    if (list.length === 0) return;
    const base = Math.floor(100 / list.length);
    const rem = 100 - (base * list.length);
    list.forEach((op, idx) => {
        opPercentages[op] = base + (idx === 0 ? rem : 0);
    });
    syncMixUI();
    markDirty();
}

function fillRemaining() {
    calcUnallocated();
    if (unallocatedPct <= 0) return;
    const list = getActiveList();
    if (list.length === 0) return;
    const base = Math.floor(unallocatedPct / list.length);
    const rem = unallocatedPct % list.length;
    list.forEach((op, idx) => {
        opPercentages[op] = (opPercentages[op] || 0) + base + (idx < rem ? 1 : 0);
    });
    syncMixUI();
    markDirty();
}

function renderLegend() {
    const listEl = el("legend_list");
    if (!listEl) return;
    const activeList = getActiveList();
    calcUnallocated();

    let itemsHtml = activeList.map(op => {
        const meta = OP_META[op] || { name: op.toUpperCase(), color: "#94a3b8" };
        const val = opPercentages[op] || 0;
        return `
      <div class="legend-item" data-op="${op}"
   onmouseenter="highlightSlice('${op}'); showHandle('${op}');" 
   onmouseleave="unhighlightSlice('${op}'); hideHandle('${op}');">
<div class="legend-left">
  <span class="legend-bar" style="background: ${meta.color}"></span>
  <span class="legend-name">${meta.name}</span>
</div>
<div class="legend-right">
  <span class="legend-badge" style="color: ${meta.color}">${val}%</span>
</div>
      </div>
    `;
    }).join("");

    // 无论未分配为 0% 还是大于 0%，恒以低调原灰色统一展示
    const unallocColor = "#64748b";
    itemsHtml += `
      <div class="legend-item unallocated-row" data-op="unallocated"
   onmouseenter="showPieTooltip('unallocated', ${unallocatedPct}, event)"
   onmouseleave="hidePieTooltip()">
<div class="legend-left">
  <span class="legend-bar" style="background: ${unallocColor};"></span>
  <span class="legend-name" style="color: var(--fg-muted);">未分配</span>
</div>
<div class="legend-right">
  <span class="legend-badge" style="color: ${unallocColor};">${unallocatedPct}%</span>
</div>
      </div>
    `;
    listEl.innerHTML = itemsHtml;

    const btnFill = el("btn_fill");
    if (btnFill) {
        if (unallocatedPct === 0) {
            btnFill.style.opacity = "0.35";
            btnFill.style.pointerEvents = "none";
            btnFill.title = "已 100% 完全分配，无需补齐";
        } else {
            btnFill.style.opacity = "1";
            btnFill.style.pointerEvents = "auto";
            btnFill.title = `一键将剩余 ${unallocatedPct}% 平均补齐到已选命令`;
        }
    }
}

function polarToCartesian(cx, cy, r, angleRad) {
    return {
        x: cx + r * Math.cos(angleRad),
        y: cy + r * Math.sin(angleRad)
    };
}

function describeSlice(cx, cy, r, startAngle, endAngle) {
    const sweep = endAngle - startAngle;
    if (sweep >= 2 * Math.PI - 0.0001) {
        return `M ${cx - r} ${cy} A ${r} ${r} 0 1 1 ${cx + r} ${cy} A ${r} ${r} 0 1 1 ${cx - r} ${cy} Z`;
    }
    const p1 = polarToCartesian(cx, cy, r, startAngle);
    const p2 = polarToCartesian(cx, cy, r, endAngle);
    const largeArc = sweep > Math.PI ? 1 : 0;
    return `M ${cx} ${cy} L ${p1.x} ${p1.y} A ${r} ${r} 0 ${largeArc} 1 ${p2.x} ${p2.y} Z`;
}

function showPieTooltip(op, pct, e) {
    if (draggingOp) return;
    const tip = el("pie_tooltip");
    const bar = el("tooltip_bar");
    const nameEl = el("tooltip_name");
    const valEl = el("tooltip_val");
    if (!tip || !bar || !nameEl || !valEl) return;

    if (op === "unallocated") {
        bar.style.backgroundColor = "#64748b";
        nameEl.textContent = "未分配";
        valEl.textContent = `${pct}%`;
    } else {
        const meta = OP_META[op];
        bar.style.backgroundColor = meta.color;
        nameEl.textContent = meta.name;
        valEl.textContent = `${pct}%`;
    }
    tip.style.display = "inline-flex";
    updateTooltipPos(e);
}

function updateTooltipPos(e) {
    const tip = el("pie_tooltip");
    const container = el("pie_container");
    if (!tip || !container) return;
    const rect = container.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    tip.style.left = `${x}px`;
    tip.style.top = `${y}px`;
}

function hidePieTooltip() {
    const tip = el("pie_tooltip");
    if (tip && !draggingOp) tip.style.display = "none";
}

function highlightSlice(op) {
    const slice = document.querySelector(`.donut-slice[data-op="${op}"]`);
    if (slice) slice.style.filter = "brightness(1.25) drop-shadow(0 0 6px rgba(255,255,255,0.3))";
}

function unhighlightSlice(op) {
    const slice = document.querySelector(`.donut-slice[data-op="${op}"]`);
    if (slice) slice.style.filter = "";
}

function showHandle(op) {
    const handle = document.querySelector(`.donut-handle[data-op="${op}"]`);
    if (handle) handle.classList.add("visible");
}

function hideHandle(op) {
    if (draggingOp === op) return;
    const handle = document.querySelector(`.donut-handle[data-op="${op}"]`);
    if (handle) handle.classList.remove("visible");
}

function renderPieChart() {
    const slicesG = el("donut_slices");
    const labelsG = el("donut_labels");
    const handlesG = el("donut_handles");
    if (!slicesG || !handlesG) return;
    slicesG.innerHTML = "";
    if (labelsG) labelsG.innerHTML = "";
    handlesG.innerHTML = "";

    const cx = 210, cy = 210, r = 186;
    const list = getActiveList();
    calcUnallocated();

    let currentAngle = -Math.PI / 2; // 12 点钟方向为起点

    list.forEach(op => {
        const pct = opPercentages[op] || 0;
        if (pct <= 0) return;
        const sweep = (pct / 100) * 2 * Math.PI;
        const nextAngle = currentAngle + sweep;

        const d = describeSlice(cx, cy, r, currentAngle, nextAngle);
        const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
        path.setAttribute("class", "donut-slice");
        path.setAttribute("d", d);
        path.setAttribute("fill", OP_META[op].color);
        path.setAttribute("data-op", op);
        path.addEventListener("mouseenter", (e) => {
            showPieTooltip(op, pct, e);
            showHandle(op);
        });
        path.addEventListener("mousemove", updateTooltipPos);
        path.addEventListener("mouseleave", () => {
            hidePieTooltip();
            hideHandle(op);
        });
        slicesG.appendChild(path);

        // 大扇区内部居中显示大写命令名称，小扇区隐藏
        if (pct >= 5 && labelsG) {
            const midAngle = currentAngle + sweep / 2;
            const labelPt = polarToCartesian(cx, cy, r * 0.62, midAngle);
            const text = document.createElementNS("http://www.w3.org/2000/svg", "text");
            text.setAttribute("class", "pie-label");
            text.setAttribute("x", labelPt.x);
            text.setAttribute("y", labelPt.y);
            text.textContent = OP_META[op].name;
            labelsG.appendChild(text);
        }

        // 实心圆周边界处的调节把手 (平时隐藏，鼠标在扇区或手柄上时出现)
        const handlePt = polarToCartesian(cx, cy, r, nextAngle);
        const handle = document.createElementNS("http://www.w3.org/2000/svg", "circle");
        handle.setAttribute("class", "donut-handle");
        handle.setAttribute("cx", handlePt.x);
        handle.setAttribute("cy", handlePt.y);
        handle.setAttribute("r", "5.5");
        handle.setAttribute("fill", "#ffffff");
        handle.setAttribute("stroke", OP_META[op].color);
        handle.setAttribute("stroke-width", "2.2");
        handle.setAttribute("data-op", op);
        handle.addEventListener("mouseenter", () => {
            showHandle(op);
            highlightSlice(op);
        });
        handle.addEventListener("mouseleave", () => {
            if (draggingOp !== op) {
                hideHandle(op);
                unhighlightSlice(op);
            }
        });
        handle.addEventListener("mousedown", (e) => startDragHandle(e, op));
        handlesG.appendChild(handle);

        currentAngle = nextAngle;
    });

    // 未分配区域展示
    if (unallocatedPct > 0) {
        const sweep = (unallocatedPct / 100) * 2 * Math.PI;
        const endAngle = currentAngle + sweep;
        const d = describeSlice(cx, cy, r, currentAngle, endAngle);
        const unallocPath = document.createElementNS("http://www.w3.org/2000/svg", "path");
        unallocPath.setAttribute("class", "donut-slice");
        unallocPath.setAttribute("d", d);
        unallocPath.setAttribute("fill", "rgba(255, 255, 255, 0.04)");
        unallocPath.setAttribute("stroke", "rgba(255, 255, 255, 0.18)");
        unallocPath.setAttribute("stroke-dasharray", "3,3");
        unallocPath.addEventListener("mouseenter", (e) => showPieTooltip("unallocated", unallocatedPct, e));
        unallocPath.addEventListener("mousemove", updateTooltipPos);
        unallocPath.addEventListener("mouseleave", hidePieTooltip);
        slicesG.appendChild(unallocPath);

        if (unallocatedPct >= 8 && labelsG) {
            const midAngle = currentAngle + sweep / 2;
            const labelPt = polarToCartesian(cx, cy, r * 0.62, midAngle);
            const text = document.createElementNS("http://www.w3.org/2000/svg", "text");
            text.setAttribute("class", "pie-label");
            text.setAttribute("style", "fill: rgba(255, 255, 255, 0.4);");
            text.setAttribute("x", labelPt.x);
            text.setAttribute("y", labelPt.y);
            text.textContent = "未分配";
            labelsG.appendChild(text);
        }
    }
}

function startDragHandle(e, op) {
    e.preventDefault();
    e.stopPropagation();
    draggingOp = op;
    const initialHandle = document.querySelector(`.donut-handle[data-op="${op}"]`);
    if (initialHandle) initialHandle.classList.add("dragging", "visible");
    const container = el("pie_container");
    const rect = container.getBoundingClientRect();
    const cx = rect.left + rect.width / 2;
    const cy = rect.top + rect.height / 2;

    let lastAngle = Math.atan2(e.clientY - cy, e.clientX - cx);
    let accumulatedAngle = 0;
    const initialPct = opPercentages[op] || 0;
    calcUnallocated();
    const maxAvailable = initialPct + unallocatedPct;

    function onMouseMove(ev) {
        const curAngle = Math.atan2(ev.clientY - cy, ev.clientX - cx);
        let diff = curAngle - lastAngle;
        while (diff > Math.PI) diff -= 2 * Math.PI;
        while (diff < -Math.PI) diff += 2 * Math.PI;

        accumulatedAngle += diff;
        lastAngle = curAngle;

        const deltaPct = Math.round((accumulatedAngle / (2 * Math.PI)) * 100);
        const targetPct = Math.max(0, Math.min(maxAvailable, initialPct + deltaPct));

        if (opPercentages[op] !== targetPct) {
            opPercentages[op] = targetPct;
            syncMixUI();
            const curH = document.querySelector(`.donut-handle[data-op="${op}"]`);
            if (curH) curH.classList.add("dragging", "visible");
        }
        showPieTooltip(op, targetPct, ev);
    }

    function onMouseUp() {
        window.removeEventListener("mousemove", onMouseMove);
        window.removeEventListener("mouseup", onMouseUp);
        const curH = document.querySelector(`.donut-handle[data-op="${op}"]`);
        if (curH) curH.classList.remove("dragging", "visible");
        draggingOp = null;
        hidePieTooltip();
        hideHandle(op);
        unhighlightSlice(op);
        markDirty();
    }

    window.addEventListener("mousemove", onMouseMove);
    window.addEventListener("mouseup", onMouseUp);
}

function syncMixUI() {
    renderCommandPills();
    renderLegend();
    renderPieChart();
}

function readForm() {
    const patch = {};
    for (const field of FIELDS) {
        if (field === "miss_ratio" || field === "ttl_ratio") continue;
        const node = el(field);
        if (!node) continue;
        patch[field] = NUMERIC.has(field) ? Number(node.value)
            : node.type === "checkbox" ? node.checked : node.value;
    }
    const hitVal = el("hit_ratio_slider") ? Number(el("hit_ratio_slider").value) : 0.9;
    patch.miss_ratio = Math.max(0, Math.min(1, Number((1 - hitVal).toFixed(2))));
    patch.ttl_ratio = el("ttl_ratio_slider") ? Number(el("ttl_ratio_slider").value) : 0;

    const slotVal = el("target_slot") ? el("target_slot").value.trim() : "";
    if (slotVal === "") {
        patch.use_hashtag = false;
        patch.target_slot = null;
    } else {
        patch.use_hashtag = true;
        patch.target_slot = Number(slotVal);
    }

    patch.mix = {};
    for (const m of ALL_OPS) {
        patch.mix[m] = activeOps.has(m) ? (opPercentages[m] || 0) : 0;
    }
    patch.mode = el("mode") ? el("mode").value : "cluster";
    const host = el("endpoint_host") ? el("endpoint_host").value.trim() : "127.0.0.1";
    patch.endpoint = `${host}:${port}`;
    return patch;
}

function renderForm(cfg) {
    for (const field of FIELDS) {
        if (field === "miss_ratio" || field === "ttl_ratio") continue;
        const node = el(field);
        if (!node || document.activeElement === node) continue;
        if (node.type === "checkbox") node.checked = Boolean(cfg[field]);
        else node.value = cfg[field];
    }
    const hitRatio = Math.max(0, Math.min(1, Number((1 - (cfg.miss_ratio ?? 0.1)).toFixed(2))));
    if (el("hit_ratio_slider") && document.activeElement !== el("hit_ratio_slider")) {
        el("hit_ratio_slider").value = hitRatio;
    }
    if (el("hit_ratio_display")) el("hit_ratio_display").textContent = Math.round(hitRatio * 100) + "%";

    const ttlRatio = cfg.ttl_ratio ?? 0;
    if (el("ttl_ratio_slider") && document.activeElement !== el("ttl_ratio_slider")) {
        el("ttl_ratio_slider").value = ttlRatio;
    }
    if (el("ttl_ratio_display")) el("ttl_ratio_display").textContent = Math.round(ttlRatio * 100) + "%";

    if (el("target_slot") && document.activeElement !== el("target_slot")) {
        if (cfg.use_hashtag && cfg.target_slot !== null && cfg.target_slot !== undefined) {
            el("target_slot").value = cfg.target_slot;
        } else if (cfg.use_hashtag) {
            el("target_slot").value = calcPrefixSlot(cfg.key_prefix);
        } else {
            el("target_slot").value = "";
        }
    }
    updateSlotPreview();

    if (cfg.mode !== undefined && document.activeElement !== el("mode")) {
        el("mode").value = cfg.mode;
        syncCustomModeDropdown();
    }
    if (cfg.endpoint !== undefined || cfg.endpoints !== undefined) {
        const parsed = splitEndpoint(cfg.endpoint || (cfg.endpoints || [])[0]);
        if (document.activeElement !== el("endpoint_host")) el("endpoint_host").value = parsed.host;
        if (document.activeElement !== el("endpoint_port")) el("endpoint_port").value = parsed.port;
    }

    if (cfg.mix && !dirty) {
        applyMixFromConfig(cfg.mix);
        syncMixUI();
    }
    currentPresetKey = detectPreset(cfg);
    updatePresetActiveCard();
}

function mixEntries(mix) {
    if (!mix) return [];
    return Object.entries(mix).filter(([key, value]) => key !== "extra" && typeof value === "number");
}

let DEFAULT_OPS = null;

function applyMixFromConfig(mix) {
    const rawEntries = mixEntries(mix);
    if (!DEFAULT_OPS) {
        DEFAULT_OPS = rawEntries.map(([key]) => key);
    }
    const activeEntries = rawEntries.filter(([, weight]) => weight > 0);
    const allSet = new Set(DEFAULT_OPS);
    for (const [key] of activeEntries) {
        allSet.add(key);
    }
    ALL_OPS = Array.from(allSet);
    activeOps = new Set(activeEntries.map(([key]) => key));
    opPercentages = {};
    const total = activeEntries.reduce((sum, [, weight]) => sum + weight, 0);
    for (const [key, weight] of activeEntries) {
        if (!OP_META[key]) {
            OP_META[key] = {
                name: key.toUpperCase().replace(/_/g, "."),
                color: COLOR_PALETTE[ALL_OPS.length % COLOR_PALETTE.length],
                desc: `客户端命令 ${key}`
            };
        }
        opPercentages[key] = total > 0 ? Math.round((weight / total) * 100) : 0;
    }
    for (const key of ALL_OPS) {
        if (!opPercentages[key]) {
            opPercentages[key] = 0;
        }
    }
}

function renderPresetCards() {
    const box = el("presets_list");
    if (!box) return;
    box.replaceChildren();
    for (const preset of PRESETS) {
        const card = document.createElement("div");
        card.className = "preset-card" + (preset.id === currentPresetKey ? " active" : "");
        card.dataset.preset = preset.id;
        card.addEventListener("click", () => applyPreset(preset.id));
        const title = document.createElement("div");
        title.className = "preset-title";
        title.textContent = preset.title || preset.id;
        const desc = document.createElement("div");
        desc.className = "preset-desc";
        desc.textContent = preset.description || preset.desc || "";
        card.append(title, desc);
        box.append(card);
    }
}

function updatePresetActiveCard() {
    document.querySelectorAll(".preset-card").forEach(card => {
        if (card.dataset.preset === currentPresetKey) card.classList.add("active");
        else card.classList.remove("active");
    });
}

function detectPreset(cfg) {
    if (!cfg || !PRESETS.length) return null;
    for (const preset of PRESETS) {
        const p = preset.config || {};
        if (cfg.target_ops !== p.target_ops ||
            cfg.connections !== p.connections ||
            cfg.pipeline !== p.pipeline ||
            (cfg.key_prefix || "") !== (p.key_prefix || "")) {
            continue;
        }
        let mixMatch = true;
        const keys = new Set([
            ...mixEntries(cfg.mix).map(([k]) => k),
            ...mixEntries(p.mix).map(([k]) => k)
        ]);
        for (const op of keys) {
            if (Number(cfg.mix?.[op] || 0) !== Number(p.mix?.[op] || 0)) {
                mixMatch = false;
                break;
            }
        }
        if (mixMatch) return preset.id;
    }
    return null;
}

async function applyPreset(key) {
    const preset = PRESETS.find(item => item.id === key);
    if (!preset || !preset.config) return;
    currentPresetKey = key;
    dirty = false;
    renderForm(preset.config);
    dirty = true;
    syncDirtyHint();
    updatePresetActiveCard();
    showToast("已应用场景预设: " + (preset.title || key));
    await patch(readForm(), true);
}

function toggleCustomMode(e) {
    if (e) e.stopPropagation();
    const dd = el("custom_mode_dropdown");
    if (dd) dd.classList.toggle("open");
}

function closeCustomMode() {
    const dd = el("custom_mode_dropdown");
    if (dd) dd.classList.remove("open");
}

function selectCustomMode(val) {
    const modeSelect = el("mode");
    if (modeSelect) {
        modeSelect.value = val;
        modeSelect.dispatchEvent(new Event("change"));
        markDirty();
    }
    syncCustomModeDropdown();
    closeCustomMode();
}

function syncCustomModeDropdown() {
    const val = el("mode") ? el("mode").value : "cluster";
    const label = el("mode_display_label");
    if (label) label.textContent = val === "single" ? "Single" : "Cluster";
    document.querySelectorAll(".custom-mode-option").forEach(opt => {
        if (opt.dataset.val === val) opt.classList.add("active");
        else opt.classList.remove("active");
    });
}

let taskInfoHideTimer = null;
let lastRtState = "stopped";

function taskInfoRow(label, value) {
    const row = document.createElement("div");
    row.className = "task-info-row";
    const k = document.createElement("span");
    k.className = "task-info-k";
    k.textContent = label;
    const v = document.createElement("span");
    v.className = "task-info-v";
    v.textContent = value;
    row.append(k, v);
    return row;
}

function taskInfoSection(title) {
    const node = document.createElement("div");
    node.className = "task-info-section";
    node.textContent = title;
    return node;
}

function taskInfoMixRow(key, weight) {
    const row = document.createElement("div");
    row.className = "task-info-row mix";
    const left = document.createElement("span");
    left.className = "legend-left";
    const bar = document.createElement("span");
    bar.className = "legend-bar";
    const meta = OP_META[key] || { name: key.toUpperCase(), color: "#94a3b8" };
    bar.style.background = meta.color;
    const name = document.createElement("span");
    name.className = "legend-name";
    name.textContent = meta.name || key.toUpperCase();
    left.append(bar, name);
    const badge = document.createElement("span");
    badge.className = "legend-badge";
    badge.style.color = meta.color;
    badge.textContent = weight + "%";
    row.append(left, badge);
    return row;
}

function fillTaskInfoPop(cfg, rt) {
    const pop = el("task_info_pop");
    const hitPct = Math.round((1 - Number(cfg.miss_ratio || 0)) * 100);
    const ttlPct = Math.round(Number(cfg.ttl_ratio || 0) * 100);
    const stateText = { running: "运行中", paused: "暂停", stopped: "未运行" }[rt.state] || rt.state;
    const slot = cfg.target_slot == null || cfg.target_slot === "" ? "全槽分散" : String(cfg.target_slot);
    const nodes = [
        (() => { const t = document.createElement("div"); t.className = "task-info-title"; t.textContent = "当前任务"; return t; })(),
        taskInfoSection("目标"),
        taskInfoRow("状态", stateText),
        taskInfoRow("拓扑", cfg.mode === "single" ? "Single" : "Cluster"),
        taskInfoRow("地址", cfg.endpoint || (cfg.endpoints || []).join(", ") || "—"),
        taskInfoSection("压力"),
        taskInfoRow("限速", cfg.target_ops > 0 ? cfg.target_ops + " ops/s" : "不限速"),
        taskInfoRow("连接", String(cfg.connections)),
        taskInfoRow("client", String(rt.workers)),
        taskInfoRow("pipeline", String(cfg.pipeline)),
        taskInfoRow("超时", cfg.timeout_ms + " ms"),
        taskInfoSection("数据"),
        taskInfoRow("前缀", cfg.key_prefix || "—"),
        taskInfoRow("键空间", String(cfg.keyspace)),
        taskInfoRow("值大小", cfg.value_size_min + " – " + cfg.value_size_max),
        taskInfoRow("命中率", hitPct + "%"),
        taskInfoRow("TTL 比例", ttlPct + "%"),
        taskInfoRow("TTL 时长", cfg.ttl_seconds + " s"),
        taskInfoRow("Hash tag", cfg.use_hashtag ? "是" : "否"),
        taskInfoRow("目标槽", slot),
        taskInfoSection("命令占比"),
    ];
    const entries = Object.entries(cfg.mix || {})
        .filter(([, w]) => Number(w) > 0)
        .sort((a, b) => Number(b[1]) - Number(a[1]));
    if (!entries.length) nodes.push(taskInfoRow("—", "空"));
    else for (const [key, weight] of entries) nodes.push(taskInfoMixRow(key, weight));
    pop.replaceChildren(...nodes);
}

function bindTaskInfoHover() {
    const state = el("state");
    const pop = el("task_info_pop");
    if (!state || !pop) return;
    const show = () => {
        if (el("task_info_btn").hidden) return;
        clearTimeout(taskInfoHideTimer);
        pop.hidden = false;
    };
    const hide = () => {
        taskInfoHideTimer = setTimeout(() => { pop.hidden = true; }, 150);
    };
    state.addEventListener("mouseenter", show);
    state.addEventListener("mouseleave", hide);
    const stop = el("task_stop_btn");
    if (stop) {
        stop.addEventListener("click", (e) => {
            e.preventDefault();
            e.stopPropagation();
            triggerTerminate();
        });
    }
}

function syncPrimaryButton(state) {
    const btn = el("btn_primary");
    const label = el("btn_primary_label");
    const icon = el("btn_primary_icon");
    if (!btn || !label || !icon) return;
    if (state === "running") {
        btn.className = "warn";
        label.textContent = "暂停";
        icon.innerHTML = '<rect x="6" y="5" width="4" height="14" rx="1"/><rect x="14" y="5" width="4" height="14" rx="1"/>';
    } else if (state === "paused") {
        btn.className = "ok";
        label.textContent = "继续";
        icon.innerHTML = '<polygon points="5 3 19 12 5 21 5 3"/>';
    } else {
        btn.className = "primary";
        label.textContent = "启动";
        icon.innerHTML = '<polygon points="5 3 19 12 5 21 5 3"/>';
    }
}

function renderRuntime(cfg, rt) {
    const labels = { running: "运行中", stopped: "未运行", paused: "暂停" };
    const stateLabel = labels[rt.state] || rt.state;
    const isOk = rt.state === "running";
    const isPaused = rt.state === "paused";
    const dotCls = isOk ? "ok" : isPaused ? "pause" : "idle";
    lastRtState = rt.state;

    el("endpoints").innerHTML = rt.endpoints.map(e =>
        `<span class="badge"><span class="dot ${e.reachable ? "ok" : "bad"}"></span>${e.addr}</span>`).join("");
    el("mode_badge").textContent = cfg.mode === "single" ? "Single" : "Cluster";
    el("state_dot").className = "dot " + dotCls;
    el("state_label").textContent = stateLabel;
    el("state").className = "badge " + (isOk ? "on" : isPaused ? "pause" : "idle");
    syncPrimaryButton(rt.state);
    syncDirtyHint();

    const live = rt.state !== "stopped";
    const info = el("task_info_btn");
    const stop = el("task_stop_btn");
    const pop = el("task_info_pop");
    info.hidden = !live;
    stop.hidden = !live;
    if (!live) {
        pop.hidden = true;
        pop.replaceChildren();
    } else {
        fillTaskInfoPop(cfg, rt);
    }

    const msg = rt.last_error ? rt.last_error.message : "";
    const stalePause = rt.state === "running" && /已暂停派发/.test(msg);
    if (msg && msg !== lastToast && !stalePause) showToast(msg);
    lastToast = msg;
}

function render(cfg, rt) { renderForm(cfg); renderRuntime(cfg, rt); }

async function refresh() {
    try {
        const res = await fetch("/api/config");
        const json = await res.json();
        if (!dirty) renderForm(json.config);
        PRESETS = json.presets || [];
        renderPresetCards();
        renderRuntime(json.config, json.runtime);
    } catch (err) { }
}

function showToast(message) {
    const node = el("toast");
    const isError = /失败|不可达|暂停|错误|拒绝|不支持|unreachable|至少需要/i.test(message);
    node.innerHTML = `
        <span class="toast-icon">
            ${isError 
                ? '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><circle cx="12" cy="12" r="10"></circle><line x1="12" y1="8" x2="12" y2="12"></line><line x1="12" y1="16" x2="12.01" y2="16"></line></svg>'
                : '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M20 6L9 17l-5-5"></path></svg>'
            }
        </span>
        <span class="toast-text"></span>
    `;
    node.querySelector(".toast-text").textContent = message;
    if (isError) node.classList.add("error");
    else node.classList.remove("error");
    node.classList.add("show");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => {
        node.classList.remove("show");
        node.classList.remove("error");
    }, 6000);
}

function markDirty() {
    dirty = true;
    syncDirtyHint();
}

function syncDirtyHint() {
    const hint = el("dirty_hint");
    if (!hint) return;
    const live = lastRtState === "running" || lastRtState === "paused";
    hint.hidden = !(live && dirty);
}

function warnIfHeavy(opKey) {
    const hint = HEAVY_HINTS[opKey];
    const dialog = el("heavy_dialog");
    const text = el("heavy_dialog_text");
    if (!hint || !dialog || !text) return;
    text.textContent = hint;
    dialog.hidden = false;
}

function closeHeavyDialog() {
    const dialog = el("heavy_dialog");
    if (dialog) dialog.hidden = true;
}

async function patch(body, keepForm) {
    try {
        const res = await fetch("/api/config", {
            method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
        });
        const json = await res.json();
        if (!res.ok) { showToast(json.error || res.statusText); return; }
        if (keepForm) { renderRuntime(json.config, json.runtime); return; }
        dirty = false;
        render(json.config, json.runtime);
    } catch (err) { showToast("请求失败: " + err.message); }
}

function triggerPrimary() {
    if (lastRtState === "running") patch({ paused: true }, true);
    else if (lastRtState === "paused") patch({ paused: false }, true);
    else patch({ ...readForm(), running: true });
}
function triggerTerminate() {
    el("task_info_pop").hidden = true;
    patch({ running: false }, true);
}

document.addEventListener("DOMContentLoaded", async () => {
    for (const field of FIELDS) {
        if (field === "miss_ratio" || field === "ttl_ratio") continue;
        if (el(field)) {
            el(field).addEventListener("input", markDirty);
            el(field).addEventListener("change", markDirty);
        }
    }
    if (el("hit_ratio_slider")) {
        el("hit_ratio_slider").addEventListener("input", markDirty);
        el("hit_ratio_slider").addEventListener("change", markDirty);
    }
    if (el("ttl_ratio_slider")) {
        el("ttl_ratio_slider").addEventListener("input", markDirty);
        el("ttl_ratio_slider").addEventListener("change", markDirty);
    }
    if (el("key_prefix")) el("key_prefix").addEventListener("input", updateSlotPreview);
    if (el("target_slot")) {
        el("target_slot").addEventListener("input", () => { updateSlotPreview(); markDirty(); });
        el("target_slot").addEventListener("change", () => { updateSlotPreview(); markDirty(); });
    }
    if (el("mode")) el("mode").addEventListener("change", markDirty);
    if (el("endpoint_host")) el("endpoint_host").addEventListener("input", markDirty);
    if (el("endpoint_port")) el("endpoint_port").addEventListener("input", markDirty);

    try {
        await loadEngineCatalog();
    } catch (err) {
        showToast("加载命令目录失败: " + err.message);
    }
    updateSlotPreview();
    syncMixUI();
    syncCustomModeDropdown();
    initCmdAddInput();
    document.addEventListener("click", (e) => {
        const dd = el("custom_mode_dropdown");
        if (dd && !dd.contains(e.target)) closeCustomMode();
    });
    bindTaskInfoHover();
    const heavy = el("heavy_dialog");
    if (heavy) {
        heavy.addEventListener("click", (e) => {
            if (e.target === heavy) closeHeavyDialog();
        });
    }
    document.addEventListener("keydown", (e) => {
        if (e.key === "Escape" && heavy && !heavy.hidden) {
            e.preventDefault();
            closeHeavyDialog();
        }
    });
    refresh();
    setInterval(refresh, 1000);
});
