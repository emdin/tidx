const PAGE_SIZE = 25;
const AUTO_REFRESH_MS = 10_000;
const PORTFOLIO_PAGE_SIZE = 12;
const TOKEN_HOLDER_PAGE_SIZE = 10;
const TOKEN_TRANSFER_PAGE_SIZE = 15;
const CONTRACT_METHOD_PAGE_SIZE = 12;

const TOPICS = {
  transfer: "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
  approval: "0x8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925",
};

const KNOWN_EVENTS = {
  [TOPICS.transfer]: "Transfer",
  [TOPICS.approval]: "Approval",
};

const KNOWN_METHODS = {
  "0x06fdde03": { label: "name()", args: [] },
  "0x095ea7b3": {
    label: "approve(address,uint256)",
    args: [
      { label: "spender", type: "address" },
      { label: "value", type: "uint256" },
    ],
  },
  "0x18160ddd": { label: "totalSupply()", args: [] },
  "0x23b872dd": {
    label: "transferFrom(address,address,uint256)",
    args: [
      { label: "from", type: "address" },
      { label: "to", type: "address" },
      { label: "value", type: "uint256" },
    ],
  },
  "0x313ce567": { label: "decimals()", args: [] },
  "0x42842e0e": {
    label: "safeTransferFrom(address,address,uint256)",
    args: [
      { label: "from", type: "address" },
      { label: "to", type: "address" },
      { label: "tokenId", type: "uint256" },
    ],
  },
  "0x70a08231": {
    label: "balanceOf(address)",
    args: [{ label: "account", type: "address" }],
  },
  "0x95d89b41": { label: "symbol()", args: [] },
  "0xa9059cbb": {
    label: "transfer(address,uint256)",
    args: [
      { label: "to", type: "address" },
      { label: "value", type: "uint256" },
    ],
  },
  "0xb88d4fde": {
    label: "safeTransferFrom(address,address,uint256,bytes)",
    args: [
      { label: "from", type: "address" },
      { label: "to", type: "address" },
      { label: "tokenId", type: "uint256" },
    ],
  },
  "0xdd62ed3e": {
    label: "allowance(address,address)",
    args: [
      { label: "owner", type: "address" },
      { label: "spender", type: "address" },
    ],
  },
};

const NATIVE_SYMBOL = "IGRA";

const state = {
  chainId: Number(window.__TIDX_DEFAULT_CHAIN_ID__) || null,
  status: null,
  adminCapabilities: null,
  refreshTimer: null,
  refreshInFlight: false,
};

const icons = {
  account:
    '<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2"><path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"></path><circle cx="12" cy="7" r="4"></circle></g></svg>',
  contract:
    '<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2"><path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z"></path><path d="M14 2v5a1 1 0 0 0 1 1h5"></path></g></svg>',
  receipt:
    '<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2"><path d="M12 17V7m4 1h-6a2 2 0 0 0 0 4h4a2 2 0 0 1 0 4H8"></path><path d="M4 3a1 1 0 0 1 1-1a1.3 1.3 0 0 1 .7.2l.933.6a1.3 1.3 0 0 0 1.4 0l.934-.6a1.3 1.3 0 0 1 1.4 0l.933.6a1.3 1.3 0 0 0 1.4 0l.933-.6a1.3 1.3 0 0 1 1.4 0l.934.6a1.3 1.3 0 0 0 1.4 0l.933-.6A1.3 1.3 0 0 1 19 2a1 1 0 0 1 1 1v18a1 1 0 0 1-1 1a1.3 1.3 0 0 1-.7-.2l-.933-.6a1.3 1.3 0 0 0-1.4 0l-.934.6a1.3 1.3 0 0 1-1.4 0l-.933-.6a1.3 1.3 0 0 0-1.4 0l-.933.6a1.3 1.3 0 0 1-1.4 0l-.934-.6a1.3 1.3 0 0 0-1.4 0l-.933.6a1.3 1.3 0 0 1-.7.2a1 1 0 0 1-1-1z"></path></g></svg>',
  blocks:
    '<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2"><path d="M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z"></path><path d="m3.3 7l8.7 5l8.7-5M12 22V12"></path></g></svg>',
  tokens:
    '<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2"><path d="M13.744 17.736a6 6 0 1 1-7.48-7.48M15 6h1v4"></path><path d="m6.134 14.768l.866-.5l2 3.464"></path><circle cx="16" cy="8" r="6"></circle></g></svg>',
  arrow:
    '<svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 12h14m-7-7l7 7l-7 7"></path></svg>',
  logs:
    '<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 19h16M4 12h16M4 5h16"></path></svg>',
};

const elements = {
  pageRoot: document.getElementById("page-root"),
  headerSearchSlot: document.getElementById("header-search-slot"),
  latestBlockLink: document.getElementById("latest-block-link"),
  latestBlockValue: document.getElementById("latest-block-value"),
};

document.addEventListener("DOMContentLoaded", () => {
  void boot();
});

async function boot() {
  try {
    await refreshStatus();
    await refreshAdminCapabilities();
    renderLatestBlock();
    installAutoRefresh();
    await renderRoute();
  } catch (error) {
    renderError(error);
  }
}

function installAutoRefresh() {
  if (state.refreshTimer) {
    clearInterval(state.refreshTimer);
  }

  state.refreshTimer = setInterval(async () => {
    if (state.refreshInFlight) {
      return;
    }

    state.refreshInFlight = true;
    try {
      const previousSynced = state.status?.synced_num ?? state.status?.tip_num ?? 0;
      await refreshStatus();
      renderLatestBlock();

      const nextSynced = state.status?.synced_num ?? state.status?.tip_num ?? 0;
      const route = currentRoute();

      if (shouldAutoRefreshRoute(route, previousSynced, nextSynced)) {
        await renderRoute({ fromAutoRefresh: true });
      }
    } catch (_error) {
      // Ignore transient refresh failures to avoid interrupting the explorer.
    } finally {
      state.refreshInFlight = false;
    }
  }, AUTO_REFRESH_MS);
}

function shouldAutoRefreshRoute(route, previousSynced, nextSynced) {
  if (!previousSynced || previousSynced === nextSynced) {
    return false;
  }

  switch (route.name) {
    case "home":
      return true;
    case "blocks":
    case "contracts":
    case "tokens":
      return route.page === 1;
    case "address":
    case "token":
      return route.page === 1;
    default:
      return false;
  }
}

async function refreshStatus() {
  const response = await fetch("/status");
  if (!response.ok) {
    throw new Error(`Failed to load status (${response.status})`);
  }

  const json = await response.json();
  const chains = Array.isArray(json.chains) ? json.chains : [];
  const matching = chains.find((chain) => Number(chain.chain_id) === Number(state.chainId));

  state.status = matching || chains[0] || null;
  if (!state.chainId && state.status) {
    state.chainId = Number(state.status.chain_id);
  }
}

async function refreshAdminCapabilities() {
  state.adminCapabilities = await tryFetchExplorerJson("/explore/api/admin/capabilities");
}

function renderLatestBlock() {
  const latest = state.status?.synced_num ?? state.status?.tip_num;
  if (!latest) {
    elements.latestBlockValue.textContent = "...";
    elements.latestBlockLink.href = "/explore/blocks";
    return;
  }

  elements.latestBlockValue.textContent = formatNumber(latest);
  elements.latestBlockLink.href = `/explore/block/${latest}`;
}

function currentRoute() {
  const prefix = "/explore";
  let path = window.location.pathname.startsWith(prefix)
    ? window.location.pathname.slice(prefix.length)
    : window.location.pathname;
  const params = new URLSearchParams(window.location.search);
  const page = parsePositiveInt(params.get("page"), 1);

  if (!path || path === "/") {
    return { name: "home", page: 1, params };
  }

  const parts = path.split("/").filter(Boolean);

  if (parts[0] === "blocks") {
    return { name: "blocks", page, params };
  }
  if (parts[0] === "contracts") {
    return { name: "contracts", page, params };
  }
  if (parts[0] === "search") {
    return { name: "search", query: params.get("q") || "", page: 1, params };
  }
  if (parts[0] === "block" && parts[1]) {
    return { name: "block", id: parts[1], page, params };
  }
  if ((parts[0] === "receipt" || parts[0] === "tx") && parts[1]) {
    return { name: "receipt", hash: normalizeHex(parts[1]), page: 1, params };
  }
  if (parts[0] === "address" && parts[1]) {
    return {
      name: "address",
      address: normalizeHex(parts[1]),
      requestedTab: params.get("tab") || "transactions",
      page,
      params,
    };
  }
  if (parts[0] === "contract" && parts[1]) {
    return {
      name: "address",
      address: normalizeHex(parts[1]),
      requestedTab: "contract",
      page,
      params,
    };
  }
  if (parts[0] === "token" && parts[1]) {
    return {
      name: "token",
      address: normalizeHex(parts[1]),
      requestedTab: "token",
      page,
      params,
    };
  }
  if (parts[0] === "tokens") {
    return { name: "tokens", page, params };
  }

  return { name: "notFound", page: 1, params };
}

async function renderRoute(_options = {}) {
  const route = currentRoute();
  const isHome = route.name === "home";
  document.body.dataset.pageMode = isHome ? "home" : "content";
  elements.headerSearchSlot.innerHTML = isHome ? "" : renderSearchForm({ compact: true });
  bindSearchForms();

  switch (route.name) {
    case "home":
      await renderHomePage();
      return;
    case "blocks":
      await renderBlocksPage(route.page);
      return;
    case "contracts":
      await renderContractsPage(route.page);
      return;
    case "search":
      await renderSearchResultsPage(route.query);
      return;
    case "block":
      await renderBlockPage(route.id, route.page);
      return;
    case "receipt":
      await renderReceiptPage(route.hash);
      return;
    case "address":
      await renderAddressPage(route.address, route.requestedTab, route.page);
      return;
    case "token":
      await renderAddressPage(route.address, "token", route.page);
      return;
    case "tokens":
      await renderTokensPage(route.page);
      return;
    default:
      setDocumentTitle("Explore - Igra");
      renderNotFound();
  }
}

async function renderHomePage() {
  setDocumentTitle("Explore - Igra");

  const recentTxs = await runQuery(`
    SELECT
      block_num,
      idx,
      encode(hash, 'hex') AS hash,
      encode("from", 'hex') AS from_addr,
      encode("to", 'hex') AS to_addr
    FROM txs
    ORDER BY block_num DESC, idx DESC
    LIMIT 5
  `);

  const recent = recentTxs[0] || {};
  const accountHref = recent.from_addr ? `/explore/address/0x${recent.from_addr}` : "/explore/blocks";
  const contractHref = recent.to_addr ? `/explore/address/0x${recent.to_addr}?tab=contract` : "/explore/blocks";
  const receiptHref = recent.hash ? `/explore/receipt/0x${recent.hash}` : "/explore/blocks";

  const latest = state.status?.synced_num ?? state.status?.tip_num ?? null;
  const lag = Math.max(Number(state.status?.lag || 0), 0);
  const liveRate = preferredSyncRate(state.status);
  const syncRate = liveRate ? `${formatRate(liveRate)} blk/s` : null;
  const chainLabel = formatChainId(state.chainId);

  const statusParts = [
    `Chain ${chainLabel}`,
    latest ? `synced to block ${formatNumber(latest)}` : null,
    lag > 0 ? `${formatNumber(lag)} lag` : null,
    syncRate,
  ].filter(Boolean);

  elements.pageRoot.innerHTML = `
    <section class="hero-page">
      <div class="hero-top-spacer">
        <div class="hero-stack">
          <span class="hero-line hero-line-subtle">Search</span>
          <span class="hero-line hero-line-mid">Explore</span>
          <span class="hero-line hero-line-main">Discover</span>
        </div>
      </div>

      <div class="hero-content">
        <div class="hero-search-wrap">
          ${renderSearchForm({ compact: false, autofocus: true })}
        </div>

        <div class="hero-status"><strong>Igra Explorer</strong> · ${escapeHtml(statusParts.join(" · "))}</div>

        <section class="pill-row" aria-label="Explore shortcuts">
          ${renderPillLink("Account", accountHref, icons.account)}
          ${renderPillLink("Contract", contractHref, icons.contract)}
          ${renderPillLink("Receipt", receiptHref, icons.receipt)}
          ${renderPillLink("Blocks", "/explore/blocks", icons.blocks)}
          ${renderPillLink("Contracts", "/explore/contracts", icons.contract)}
          ${renderPillLink("Tokens", "/explore/tokens", icons.tokens)}
        </section>
      </div>
    </section>
  `;

  bindSearchForms();
}

async function renderBlocksPage(page) {
  setDocumentTitle("Blocks · Igra Explorer");

  const offset = (page - 1) * PAGE_SIZE;
  const blocks = await runQuery(`
    SELECT
      b.num,
      encode(b.hash, 'hex') AS hash,
      b.timestamp,
      b.gas_used,
      b.gas_limit,
      (SELECT COUNT(*) FROM txs t WHERE t.block_num = b.num) AS tx_count
    FROM blocks b
    ORDER BY b.num DESC
    LIMIT ${PAGE_SIZE}
    OFFSET ${offset}
  `);

  const latest = state.status?.synced_num ?? state.status?.tip_num ?? 0;
  const chainLabel = formatChainId(state.chainId);

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <span class="status-pill">Live</span>
          <span class="muted-badge mono">Chain ${chainLabel}</span>
          <span class="muted-badge mono">Head ${formatNumber(latest)}</span>
        </div>
        <div>
          <h1 class="page-heading">Latest blocks</h1>
          <p class="page-subheading">Tempo-style explorer view backed directly by your local tidx index.</p>
        </div>
      </header>

      <section class="panel-card">
        <div class="panel-header">
          <div>
            <div class="panel-title">Blocks</div>
            <div class="panel-subtitle">Recent blocks with transaction counts and gas usage</div>
          </div>
          <div class="section-actions">
            <a class="ghost-link" href="/explore">${icons.arrow} Search</a>
          </div>
        </div>
        <div class="panel-body table-wrap">
          ${renderBlocksTable(blocks)}
        </div>
        ${renderPagination("/explore/blocks", page, blocks.length === PAGE_SIZE)}
      </section>
    </section>
  `;
}

async function renderSearchResultsPage(rawQuery) {
  const query = String(rawQuery || "").trim();
  setDocumentTitle(query ? `${query} · Search · Igra Explorer` : "Search · Igra Explorer");

  if (!query) {
    elements.pageRoot.innerHTML = `
      <section class="content-page">
        <section class="not-found-card">
          <div class="not-found-copy">Enter an address, transaction hash, block number, label, or token symbol to search.</div>
        </section>
      </section>
    `;
    return;
  }

  const payload = await fetchExplorerJson(
    `/explore/api/search?chainId=${encodeURIComponent(state.chainId)}&q=${encodeURIComponent(query)}`,
  );
  const results = Array.isArray(payload.results) ? payload.results : [];

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <a class="muted-badge" href="/explore">${icons.arrow} Search</a>
          <span class="muted-badge">${formatNumber(results.length)} result(s)</span>
        </div>
        <div>
          <h1 class="page-heading">Search results</h1>
          <p class="page-subheading mono wrap-anywhere">${escapeHtml(query)}</p>
        </div>
      </header>

      <section class="panel-card">
        <div class="panel-header">
          <div>
            <div class="panel-title">Explorer search</div>
            <div class="panel-subtitle">Blocks, transactions, labels, contracts, and token metadata</div>
          </div>
        </div>
        <div class="panel-body">
          ${renderSearchResultsTable(results)}
        </div>
      </section>
    </section>
  `;
}

async function renderBlockPage(blockId, page) {
  const blockNum = Number(blockId);
  if (!Number.isFinite(blockNum)) {
    renderNotFound("Invalid block number.");
    return;
  }

  const offset = (page - 1) * PAGE_SIZE;
  const [blocks, txs, counts] = await Promise.all([
    runQuery(`
      SELECT
        num,
        encode(hash, 'hex') AS hash,
        encode(parent_hash, 'hex') AS parent_hash,
        timestamp,
        gas_used,
        gas_limit,
        encode(miner, 'hex') AS miner
      FROM blocks
      WHERE num = ${blockNum}
      LIMIT 1
    `),
    runQuery(`
      SELECT
        txs.block_num,
        txs.block_timestamp,
        txs.idx,
        encode(txs.hash, 'hex') AS hash,
        encode(txs."from", 'hex') AS from_addr,
        encode(txs."to", 'hex') AS to_addr,
        txs.gas_used,
        txs.value,
        receipts.status
      FROM txs
      LEFT JOIN receipts ON receipts.tx_hash = txs.hash
      WHERE txs.block_num = ${blockNum}
      ORDER BY txs.idx ASC
      LIMIT ${PAGE_SIZE}
      OFFSET ${offset}
    `),
    runQuery(`
      SELECT
        (SELECT COUNT(*) FROM txs WHERE block_num = ${blockNum}) AS tx_count,
        (SELECT COUNT(*) FROM logs WHERE block_num = ${blockNum}) AS log_count,
        (SELECT COUNT(*) FROM receipts WHERE block_num = ${blockNum}) AS receipt_count
    `),
  ]);

  const block = blocks[0];
  if (!block) {
    renderNotFound(`Block ${escapeHtml(blockId)} was not found in the local index.`);
    return;
  }

  setDocumentTitle(`Block ${block.num} · Igra Explorer`);

  const stats = counts[0] || {};
  const latest = state.status?.synced_num ?? state.status?.tip_num ?? 0;
  const nextBlockHref = blockNum < latest ? `/explore/block/${blockNum + 1}` : null;
  const previousBlockHref = blockNum > 0 ? `/explore/block/${blockNum - 1}` : null;

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <a class="muted-badge" href="/explore/blocks">${icons.blocks} Back to blocks</a>
          ${nextBlockHref ? `<a class="muted-badge" href="${nextBlockHref}">Next</a>` : ""}
          ${previousBlockHref ? `<a class="muted-badge" href="${previousBlockHref}">Previous</a>` : ""}
        </div>
        <div>
          <h1 class="page-heading">Block ${formatNumber(block.num)}</h1>
          <p class="page-subheading">${escapeHtml(formatTimestamp(block.timestamp))}</p>
        </div>
      </header>

      <section class="kpi-grid">
        ${renderKpiCard("Transactions", formatNumber(stats.tx_count || 0), "Included in this block")}
        ${renderKpiCard("Logs", formatNumber(stats.log_count || 0), "Decoded raw log rows")}
        ${renderKpiCard("Receipts", formatNumber(stats.receipt_count || 0), "Receipt rows indexed")}
      </section>

      <div class="page-grid">
        <aside class="summary-card">
          ${renderSummaryRow("Block", formatNumber(block.num))}
          ${renderSummaryRow("Hash", renderMonoLink(`/explore/block/${block.num}`, with0x(block.hash), false))}
          ${renderSummaryRow("Parent", previousBlockHref ? renderMonoLink(previousBlockHref, with0x(block.parent_hash), false) : escapeHtml(with0x(block.parent_hash)))}
          ${renderSummaryRow("Miner", renderMonoLink(`/explore/address/${with0x(block.miner)}?tab=contract`, with0x(block.miner), false))}
          ${renderSummaryRow("Gas used", formatGasUnits(block.gas_used))}
          ${renderSummaryRow("Gas limit", formatGasUnits(block.gas_limit))}
        </aside>

        <section class="panel-card">
          <div class="panel-header">
            <div>
              <div class="panel-title">Transactions</div>
              <div class="panel-subtitle">${formatNumber(stats.tx_count || 0)} transaction(s) in block ${formatNumber(block.num)}</div>
            </div>
          </div>
          <div class="panel-body table-wrap">
            ${renderBlockTransactionsTable(txs)}
          </div>
          ${page > 1 || txs.length === PAGE_SIZE ? renderPagination(`/explore/block/${block.num}`, page, txs.length === PAGE_SIZE) : ""}
        </section>
      </div>
    </section>
  `;

  bindReadContractForms();
}

async function renderReceiptPage(hash) {
  if (!isHex(hash, 64)) {
    renderNotFound("Invalid transaction hash.");
    return;
  }

  const body = hexBody(hash);
  const [txRows, logRows, decodePayload] = await Promise.all([
    runQuery(`
      SELECT
        txs.block_num,
        txs.block_timestamp,
        txs.idx,
        encode(txs.hash, 'hex') AS hash,
        CASE
          WHEN octet_length(txs.input) >= 4 THEN encode(substring(txs.input FROM 1 FOR 4), 'hex')
          ELSE NULL
        END AS selector,
        encode(txs.input, 'hex') AS input_data,
        encode(txs."from", 'hex') AS from_addr,
        encode(txs."to", 'hex') AS to_addr,
        txs.value,
        txs.gas_limit,
        txs.gas_used,
        txs.nonce,
        txs.type,
        receipts.status,
        receipts.effective_gas_price,
        receipts.cumulative_gas_used,
        encode(receipts.contract_address, 'hex') AS contract_address
      FROM txs
      LEFT JOIN receipts ON receipts.tx_hash = txs.hash
      WHERE txs.hash = decode('${body}', 'hex')
      LIMIT 1
    `),
    runQuery(`
      SELECT
        log_idx,
        encode(address, 'hex') AS address,
        encode(topic0, 'hex') AS topic0,
        encode(topic1, 'hex') AS topic1,
        encode(topic2, 'hex') AS topic2,
        encode(topic3, 'hex') AS topic3,
        octet_length(data) AS data_length
      FROM logs
      WHERE tx_hash = decode('${body}', 'hex')
      ORDER BY log_idx ASC
      LIMIT 250
    `),
    tryFetchExplorerJson(
      `/explore/api/receipt/${hash}/decode?chainId=${encodeURIComponent(state.chainId)}`,
    ),
  ]);

  const tx = txRows[0];
  if (!tx) {
    renderNotFound(`Receipt ${escapeHtml(hash)} was not found in the local index.`);
    return;
  }

  setDocumentTitle(`Receipt ${shortHex(hash)} · Igra Explorer`);

  const statusKind = tx.status === null ? "pending" : Number(tx.status) === 1 ? "success" : "failed";
  const gasFee = multiplyNumericStrings(tx.gas_used, tx.effective_gas_price);
  const decodedInput = decodePayload?.call || decodeCallData(tx.input_data);
  const selector = decodedInput?.selector || (tx.selector ? with0x(tx.selector) : null);
  const methodLabel = decodedInput?.label || methodName(selector);
  const inputLength = tx.input_data ? Math.max(0, tx.input_data.length / 2) : 0;

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <a class="muted-badge" href="/explore/blocks">${icons.blocks} Blocks</a>
          ${renderTxStatusBadge(statusKind)}
        </div>
        <div>
          <h1 class="page-heading">Receipt</h1>
          <p class="page-subheading mono wrap-anywhere">${escapeHtml(hash)}</p>
        </div>
      </header>

      <section class="kpi-grid">
        ${renderKpiCard("Logs", formatNumber(logRows.length), "Event rows emitted")}
        ${renderKpiCard("Gas used", formatGasUnits(tx.gas_used), "Execution gas consumed")}
        ${renderKpiCard("Fee", gasFee ? formatNativeAmount(gasFee, 8) : "-", "gas_used × effective_gas_price")}
        ${renderKpiCard("Method", methodLabel || "-", selector ? selector : "No calldata selector")}
      </section>

      <div class="page-grid">
        <aside class="summary-card">
          ${renderSummaryRow("Status", renderTxStatusBadge(statusKind))}
          ${renderSummaryRow("Block", `<a href="/explore/block/${tx.block_num}">${formatNumber(tx.block_num)}</a>`)}
          ${renderSummaryRow("Time", escapeHtml(formatTimestamp(tx.block_timestamp)))}
          ${renderSummaryRow("Index", formatNumber(tx.idx))}
          ${renderSummaryRow("From", renderMonoLink(`/explore/address/${with0x(tx.from_addr)}`, with0x(tx.from_addr), false))}
          ${renderSummaryRow("To", tx.to_addr ? renderMonoLink(`/explore/address/${with0x(tx.to_addr)}`, with0x(tx.to_addr), false) : '<span class="text-secondary">Contract creation</span>')}
          ${renderSummaryRow("Contract", tx.contract_address ? renderMonoLink(`/explore/address/${with0x(tx.contract_address)}?tab=contract`, with0x(tx.contract_address), false) : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Nonce", formatNumber(tx.nonce))}
          ${renderSummaryRow("Type", formatNumber(tx.type))}
          ${renderSummaryRow("Value", formatNativeAmount(tx.value || "0", 8))}
          ${renderSummaryRow("Selector", selector ? `<span class="wrap-anywhere">${escapeHtml(selector)}</span>` : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Gas limit", formatGasUnits(tx.gas_limit))}
          ${renderSummaryRow("Gas price", tx.effective_gas_price ? formatGasPrice(tx.effective_gas_price) : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Cumulative gas", tx.cumulative_gas_used ? formatGasUnits(tx.cumulative_gas_used) : '<span class="text-secondary">-</span>')}
        </aside>

        <div class="panel-stack">
          <section class="panel-card">
            <div class="panel-header">
              <div>
                <div class="panel-title">Function input</div>
                <div class="panel-subtitle">${methodLabel || "Raw calldata"} · ${formatNumber(inputLength)} byte(s)</div>
              </div>
            </div>
            <div class="panel-body">
              ${renderInputPanel(tx.input_data, decodedInput)}
            </div>
          </section>

          <section class="panel-card">
            <div class="panel-header">
              <div>
                <div class="panel-title">Logs</div>
                <div class="panel-subtitle">${formatNumber(logRows.length)} log(s) recorded for this receipt</div>
              </div>
            </div>
            <div class="panel-body table-wrap">
              ${renderLogsTable(logRows, decodePayload?.logs || [])}
            </div>
          </section>
        </div>
      </div>
    </section>
  `;
}

async function renderAddressPage(address, requestedTab, page) {
  if (!isHex(address, 40)) {
    renderNotFound("Invalid address.");
    return;
  }

  const body = hexBody(address);
  const padded = paddedAddressTopic(address);

  const [summaryRows, inspect] = await Promise.all([
    runQuery(`
      SELECT
        (SELECT COUNT(*) FROM txs WHERE "from" = decode('${body}', 'hex') OR "to" = decode('${body}', 'hex')) AS tx_count,
        (SELECT COUNT(*) FROM txs WHERE "from" = decode('${body}', 'hex')) AS sent_count,
        (SELECT COUNT(*) FROM txs WHERE "to" = decode('${body}', 'hex')) AS received_count,
        (SELECT COUNT(*) FROM logs WHERE address = decode('${body}', 'hex') OR topic1 = decode('${padded}', 'hex') OR topic2 = decode('${padded}', 'hex') OR topic3 = decode('${padded}', 'hex')) AS related_logs,
        (SELECT MIN(block_num) FROM (
          SELECT block_num FROM txs WHERE "from" = decode('${body}', 'hex') OR "to" = decode('${body}', 'hex')
          UNION ALL
          SELECT block_num FROM receipts WHERE contract_address = decode('${body}', 'hex')
          UNION ALL
          SELECT block_num FROM logs WHERE address = decode('${body}', 'hex')
        ) AS seen) AS first_seen_block,
        (SELECT MAX(block_num) FROM (
          SELECT block_num FROM txs WHERE "from" = decode('${body}', 'hex') OR "to" = decode('${body}', 'hex')
          UNION ALL
          SELECT block_num FROM receipts WHERE contract_address = decode('${body}', 'hex')
          UNION ALL
          SELECT block_num FROM logs WHERE address = decode('${body}', 'hex')
        ) AS seen) AS last_seen_block,
        EXISTS(SELECT 1 FROM receipts WHERE contract_address = decode('${body}', 'hex')) AS was_created,
        (SELECT MIN(block_num) FROM receipts WHERE contract_address = decode('${body}', 'hex')) AS created_block,
        (SELECT COUNT(*) FROM logs WHERE address = decode('${body}', 'hex')) AS emitted_logs,
        (SELECT COUNT(*) FROM logs WHERE address = decode('${body}', 'hex') AND topic0 = decode('${hexBody(TOPICS.transfer)}', 'hex')) AS transfer_logs,
        (SELECT COUNT(*) FROM logs WHERE address = decode('${body}', 'hex') AND topic0 = decode('${hexBody(TOPICS.approval)}', 'hex')) AS approval_logs
    `),
    tryFetchExplorerJson(`/explore/api/address/${address}/inspect?chainId=${encodeURIComponent(state.chainId)}`),
  ]);

  const summary = summaryRows[0] || {};
  const profile = inspect?.profile || null;
  const label = inspect?.label || null;
  const kind = classifyAddress(summary, profile);
  const tab = normalizeAddressTab(requestedTab, kind);

  setDocumentTitle(`${label?.label || kind.title} ${shortHex(address)} · Igra Explorer`);

  let panelTitle = "Transactions";
  let panelSubtitle = "";
  let panelBody = "";
  let paginationPath = `/explore/address/${address}`;
  let paginationExtras = { tab };
  let hasNextPage = false;
  let contractVerification = null;

  if (tab === "transactions") {
    const offset = (page - 1) * PAGE_SIZE;
    const txRows = await runQuery(`
      SELECT
        txs.block_num,
        txs.block_timestamp,
        txs.idx,
        encode(txs.hash, 'hex') AS hash,
        encode(txs."from", 'hex') AS from_addr,
        encode(txs."to", 'hex') AS to_addr,
        txs.value,
        txs.gas_used,
        receipts.status
      FROM txs
      LEFT JOIN receipts ON receipts.tx_hash = txs.hash
      WHERE txs."from" = decode('${body}', 'hex') OR txs."to" = decode('${body}', 'hex')
      ORDER BY txs.block_num DESC, txs.idx DESC
      LIMIT ${PAGE_SIZE}
      OFFSET ${offset}
    `);

    panelTitle = "Transactions";
    panelSubtitle = `${formatNumber(summary.tx_count || 0)} transaction(s) indexed for this address`;
    panelBody = renderAddressTransactionsTable(txRows, address);
    hasNextPage = txRows.length === PAGE_SIZE;
  } else if (tab === "portfolio") {
    const portfolio = await tryFetchExplorerJson(
      `/explore/api/address/${address}/portfolio?chainId=${encodeURIComponent(state.chainId)}&page=${page}&limit=${PORTFOLIO_PAGE_SIZE}`,
    );
    panelTitle = "Portfolio";
    panelSubtitle = "Native balance and token balances inferred from indexed Transfer logs";
    panelBody = renderPortfolioPanel(portfolio, profile);
    hasNextPage = Boolean(portfolio?.holdings?.length === PORTFOLIO_PAGE_SIZE);
  } else if (tab === "logs") {
    const offset = (page - 1) * PAGE_SIZE;
    const logRows = await runQuery(`
      SELECT
        block_num,
        block_timestamp,
        log_idx,
        tx_idx,
        encode(tx_hash, 'hex') AS tx_hash,
        encode(address, 'hex') AS contract,
        encode(topic0, 'hex') AS topic0,
        encode(topic1, 'hex') AS topic1,
        encode(topic2, 'hex') AS topic2,
        encode(topic3, 'hex') AS topic3,
        CASE
          WHEN address = decode('${body}', 'hex') THEN 'emitted'
          WHEN topic1 = decode('${padded}', 'hex') OR topic2 = decode('${padded}', 'hex') OR topic3 = decode('${padded}', 'hex') THEN 'indexed'
          ELSE 'related'
        END AS relation
      FROM logs
      WHERE address = decode('${body}', 'hex')
         OR topic1 = decode('${padded}', 'hex')
         OR topic2 = decode('${padded}', 'hex')
         OR topic3 = decode('${padded}', 'hex')
      ORDER BY block_num DESC, log_idx DESC
      LIMIT ${PAGE_SIZE}
      OFFSET ${offset}
    `);

    panelTitle = "Logs";
    panelSubtitle = `${formatNumber(summary.related_logs || 0)} related log row(s)`;
    panelBody = renderAddressLogsTable(logRows);
    hasNextPage = logRows.length === PAGE_SIZE;
  } else if (tab === "contract") {
    const [contractLogs, methods, verification] = await Promise.all([
      runQuery(`
        SELECT
          block_num,
          block_timestamp,
          log_idx,
          encode(tx_hash, 'hex') AS tx_hash,
          encode(topic0, 'hex') AS topic0
        FROM logs
        WHERE address = decode('${body}', 'hex')
        ORDER BY block_num DESC, log_idx DESC
        LIMIT 12
      `),
      tryFetchExplorerJson(
        `/explore/api/contract/${address}/methods?chainId=${encodeURIComponent(state.chainId)}&page=${page}&limit=${CONTRACT_METHOD_PAGE_SIZE}`,
      ),
      tryFetchExplorerJson(
        `/explore/api/contract/${address}/verification?chainId=${encodeURIComponent(state.chainId)}`,
      ),
    ]);

    panelTitle = "Contract";
    panelSubtitle = kind.isContract
      ? "Live bytecode inspection plus indexed method and event activity"
      : "This address does not currently look like a contract from indexed data";
    contractVerification = verification;
    panelBody = renderContractPanel(address, kind, summary, contractLogs, inspect, methods, verification);
    hasNextPage = Boolean(methods?.methods?.length === CONTRACT_METHOD_PAGE_SIZE);
  } else {
    const [tokenSummaryRows, tokenView, tokenTransfers, tokenApprovals] = await Promise.all([
      runQuery(`
        SELECT
          (SELECT COUNT(*) FROM logs WHERE address = decode('${body}', 'hex') AND topic0 = decode('${hexBody(TOPICS.transfer)}', 'hex')) AS transfer_count,
          (SELECT COUNT(*) FROM logs WHERE address = decode('${body}', 'hex') AND topic0 = decode('${hexBody(TOPICS.approval)}', 'hex')) AS approval_count,
          (SELECT COUNT(*) FROM (
            SELECT DISTINCT topic1 FROM logs WHERE address = decode('${body}', 'hex') AND topic0 = decode('${hexBody(TOPICS.transfer)}', 'hex') AND topic1 IS NOT NULL
            UNION
            SELECT DISTINCT topic2 FROM logs WHERE address = decode('${body}', 'hex') AND topic0 = decode('${hexBody(TOPICS.transfer)}', 'hex') AND topic2 IS NOT NULL
          ) AS token_participants) AS participant_count
      `),
      tryFetchExplorerJson(
        `/explore/api/token/${address}/holders?chainId=${encodeURIComponent(state.chainId)}&page=${page}&limit=${TOKEN_HOLDER_PAGE_SIZE}`,
      ),
      tryFetchExplorerJson(
        `/explore/api/token/${address}/transfers?chainId=${encodeURIComponent(state.chainId)}&page=${page}&limit=${TOKEN_TRANSFER_PAGE_SIZE}`,
      ),
      tryFetchExplorerJson(
        `/explore/api/token/${address}/approvals?chainId=${encodeURIComponent(state.chainId)}&limit=10`,
      ),
    ]);

    const tokenSummary = tokenSummaryRows[0] || {};
    panelTitle = "Token";
    panelSubtitle = kind.isToken
      ? "Metadata, top holders, recent transfers, and approvals"
      : "This address is not currently classified as token-like";
    panelBody = renderTokenPanel(kind, tokenSummary, inspect, tokenView, tokenTransfers, tokenApprovals);
    hasNextPage = Boolean(tokenTransfers?.transfers?.length === TOKEN_TRANSFER_PAGE_SIZE || tokenView?.holders?.length === TOKEN_HOLDER_PAGE_SIZE);
  }

  const adminPanel = state.adminCapabilities?.can_write_metadata
    ? renderExplorerAdminPanel(address, kind, inspect, contractVerification)
    : "";

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <a class="muted-badge" href="/explore">${icons.arrow} Search</a>
          <span class="muted-badge">${kind.badge}</span>
          ${kind.isToken ? `<a class="muted-badge" href="/explore/token/${address}">${icons.tokens} Token view</a>` : ""}
        </div>
        <div>
          <h1 class="page-heading">${escapeHtml(label?.label || kind.title)}</h1>
          <p class="page-subheading mono wrap-anywhere">${escapeHtml(address)}</p>
        </div>
      </header>

      <section class="kpi-grid">
        ${renderKpiCard("Transactions", formatNumber(summary.tx_count || 0), "Direct tx activity")}
        ${renderKpiCard("Logs", formatNumber(summary.related_logs || 0), "Logs emitted or indexed to this address")}
        ${renderKpiCard("Native balance", profile ? formatNativeAmount(profile.native_balance || "0", 6) : "-", "Latest RPC balance")}
        ${renderKpiCard("First seen", summary.first_seen_block ? formatNumber(summary.first_seen_block) : "-", "Earliest indexed block")}
        ${renderKpiCard("Last seen", summary.last_seen_block ? formatNumber(summary.last_seen_block) : "-", "Latest indexed block")}
      </section>

      <div class="page-grid">
        <aside class="summary-card">
          ${renderSummaryRow("Address", escapeHtml(address))}
          ${renderSummaryRow("Label", label?.label ? escapeHtml(label.label) : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Kind", escapeHtml(kind.badge))}
          ${renderSummaryRow("Detected kind", profile?.detected_kind ? escapeHtml(profile.detected_kind.toUpperCase()) : '<span class="text-secondary">Indexed only</span>')}
          ${renderSummaryRow("Verification", formatVerificationStatus(inspect?.verification?.verification_status))}
          ${renderSummaryRow("Bytecode proof", formatBytecodeProof(inspect?.verification))}
          ${renderSummaryRow("Native balance", profile ? formatNativeAmount(profile.native_balance || "0", 6) : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Transactions", formatNumber(summary.tx_count || 0))}
          ${renderSummaryRow("Sent", formatNumber(summary.sent_count || 0))}
          ${renderSummaryRow("Received", formatNumber(summary.received_count || 0))}
          ${renderSummaryRow("First seen", summary.first_seen_block ? `<a href="/explore/block/${summary.first_seen_block}">${formatNumber(summary.first_seen_block)}</a>` : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Last seen", summary.last_seen_block ? `<a href="/explore/block/${summary.last_seen_block}">${formatNumber(summary.last_seen_block)}</a>` : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Created as contract", summary.was_created ? "Yes" : "No")}
          ${renderSummaryRow("Created block", summary.created_block ? `<a href="/explore/block/${summary.created_block}">${formatNumber(summary.created_block)}</a>` : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Bytecode", profile?.is_contract ? `${formatNumber(profile.bytecode_size || 0)} bytes` : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Emitted logs", formatNumber(summary.emitted_logs || 0))}
          ${renderSummaryRow("Transfer logs", formatNumber(summary.transfer_logs || 0))}
          ${renderSummaryRow("Approval logs", formatNumber(summary.approval_logs || 0))}
        </aside>

        <section class="panel-card">
          <div class="panel-header panel-header-stack">
            <div>
              <div class="panel-title">${escapeHtml(panelTitle)}</div>
              <div class="panel-subtitle">${escapeHtml(panelSubtitle)}</div>
            </div>
            <nav class="tab-bar" aria-label="Address sections">
              ${renderAddressTabs(address, tab, kind)}
            </nav>
          </div>
          <div class="panel-body">
            ${panelBody}
          </div>
          ${page > 1 || hasNextPage ? renderPagination(paginationPath, page, hasNextPage, paginationExtras) : ""}
        </section>
      </div>
      ${adminPanel}
    </section>
  `;

  bindReadContractForms();
  bindExplorerAdminForms();
}

async function renderTokensPage(page) {
  setDocumentTitle("Tokens · Igra Explorer");
  const payload = await fetchExplorerJson(
    `/explore/api/tokens?chainId=${encodeURIComponent(state.chainId)}&page=${page}&limit=${PAGE_SIZE}`,
  );
  const tokenRows = Array.isArray(payload?.tokens) ? payload.tokens : [];

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <span class="status-pill">Indexed</span>
          <span class="muted-badge">Metadata cache + Transfer/Approval activity</span>
        </div>
        <div>
          <h1 class="page-heading">Tokens</h1>
          <p class="page-subheading">Token-like contracts ranked by cached metadata, official labels, and indexed activity.</p>
        </div>
      </header>

      <section class="panel-card">
        <div class="panel-header">
          <div>
            <div class="panel-title">Token directory</div>
            <div class="panel-subtitle">Addresses emitting ERC-style Transfer or Approval events with explorer metadata when available</div>
          </div>
        </div>
        <div class="panel-body table-wrap">
          ${renderTokensTable(tokenRows)}
        </div>
        ${renderPagination("/explore/tokens", page, tokenRows.length === PAGE_SIZE)}
      </section>
    </section>
  `;
}

async function renderContractsPage(page) {
  setDocumentTitle("Contracts · Igra Explorer");
  const payload = await fetchExplorerJson(
    `/explore/api/contracts?chainId=${encodeURIComponent(state.chainId)}&page=${page}&limit=${PAGE_SIZE}`,
  );
  const contractRows = Array.isArray(payload?.contracts) ? payload.contracts : [];

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <span class="status-pill">Verified</span>
          <span class="muted-badge">Imported from canonical Igra Blockscout</span>
        </div>
        <div>
          <h1 class="page-heading">Contracts</h1>
          <p class="page-subheading">Verified contracts with stored ABI and source available to the explorer.</p>
        </div>
      </header>

      <section class="panel-card">
        <div class="panel-header">
          <div>
            <div class="panel-title">Verified contract directory</div>
            <div class="panel-subtitle">Legacy and newly verified contracts imported from explorer.igralabs.com</div>
          </div>
        </div>
        <div class="panel-body table-wrap">
          ${renderContractsTable(contractRows)}
        </div>
        ${renderPagination("/explore/contracts", page, contractRows.length === PAGE_SIZE)}
      </section>
    </section>
  `;
}

function renderBlocksTable(rows) {
  if (!rows.length) {
    return '<div class="empty-state">No blocks indexed yet.</div>';
  }

  const body = rows
    .map(
      (row) => `
        <tr>
          <td class="mono"><a href="/explore/block/${row.num}">${formatNumber(row.num)}</a></td>
          <td>${escapeHtml(formatRelativeTime(row.timestamp))}</td>
          <td class="align-right mono">${formatNumber(row.tx_count || 0)}</td>
          <td class="mono"><a href="/explore/block/${row.num}">${escapeHtml(shortHex(with0x(row.hash), 10))}</a></td>
          <td class="align-right mono">${formatNumber(row.gas_used)}</td>
        </tr>
      `,
    )
    .join("");

  return `
    <table class="data-table">
      <thead>
        <tr>
          <th>Block</th>
          <th>Time</th>
          <th class="align-right">Txs</th>
          <th>Hash</th>
          <th class="align-right">Gas used</th>
        </tr>
      </thead>
      <tbody>${body}</tbody>
    </table>
  `;
}

function renderBlockTransactionsTable(rows) {
  if (!rows.length) {
    return '<div class="empty-state">No transactions found in this block.</div>';
  }

  const body = rows
    .map((row) => {
      const toAddress = row.to_addr ? with0x(row.to_addr) : null;
      return `
        <tr>
          <td>${renderTxStatusBadge(statusKindFromValue(row.status))}</td>
          <td class="mono"><a href="/explore/receipt/${with0x(row.hash)}">${escapeHtml(shortHex(with0x(row.hash), 10))}</a></td>
          <td class="mono"><a href="/explore/address/${with0x(row.from_addr)}">${escapeHtml(shortHex(with0x(row.from_addr), 8))}</a></td>
          <td class="mono">${toAddress ? `<a href="/explore/address/${toAddress}">${escapeHtml(shortHex(toAddress, 8))}</a>` : '<span class="text-secondary">create</span>'}</td>
          <td class="align-right mono">${formatNativeAmount(row.value || "0", 6)}</td>
          <td class="align-right mono">${formatNumber(row.gas_used)}</td>
        </tr>
      `;
    })
    .join("");

  return `
    <table class="data-table">
      <thead>
        <tr>
          <th>Status</th>
          <th>Hash</th>
          <th>From</th>
          <th>To</th>
          <th class="align-right">Value</th>
          <th class="align-right">Gas used</th>
        </tr>
      </thead>
      <tbody>${body}</tbody>
    </table>
  `;
}

function renderAddressTransactionsTable(rows, address) {
  if (!rows.length) {
    return '<div class="empty-state">No address activity found.</div>';
  }

  const normalized = normalizeHex(address);
  const body = rows
    .map((row) => {
      const from = with0x(row.from_addr);
      const to = row.to_addr ? with0x(row.to_addr) : null;
      const direction = normalizeHex(from) === normalized ? "OUT" : "IN";
      const counterparty = direction === "OUT" ? to : from;

      return `
        <tr>
          <td>${renderTxStatusBadge(statusKindFromValue(row.status))}</td>
          <td class="mono"><a href="/explore/block/${row.block_num}">${formatNumber(row.block_num)}</a></td>
          <td>${escapeHtml(formatRelativeTime(row.block_timestamp))}</td>
          <td><span class="direction-chip direction-chip-${direction.toLowerCase()}">${direction}</span></td>
          <td class="mono"><a href="/explore/receipt/${with0x(row.hash)}">${escapeHtml(shortHex(with0x(row.hash), 10))}</a></td>
          <td class="mono">${counterparty ? `<a href="/explore/address/${counterparty}">${escapeHtml(shortHex(counterparty, 8))}</a>` : '<span class="text-secondary">-</span>'}</td>
          <td class="align-right mono">${formatNativeAmount(row.value || "0", 6)}</td>
        </tr>
      `;
    })
    .join("");

  return `
    <div class="table-wrap">
      <table class="data-table">
        <thead>
          <tr>
            <th>Status</th>
            <th>Block</th>
            <th>Time</th>
            <th>Dir</th>
            <th>Hash</th>
            <th>Counterparty</th>
            <th class="align-right">Value</th>
          </tr>
        </thead>
        <tbody>${body}</tbody>
      </table>
    </div>
  `;
}

function renderAddressLogsTable(rows) {
  if (!rows.length) {
    return '<div class="empty-state">No related logs found.</div>';
  }

  const body = rows
    .map(
      (row) => `
        <tr>
          <td class="mono"><a href="/explore/block/${row.block_num}">${formatNumber(row.block_num)}</a></td>
          <td>${escapeHtml(formatRelativeTime(row.block_timestamp))}</td>
          <td><span class="relation-chip">${escapeHtml(row.relation)}</span></td>
          <td class="mono"><a href="/explore/receipt/${with0x(row.tx_hash)}">${escapeHtml(shortHex(with0x(row.tx_hash), 10))}</a></td>
          <td class="mono"><a href="/explore/address/${with0x(row.contract)}?tab=contract">${escapeHtml(shortHex(with0x(row.contract), 8))}</a></td>
          <td>${escapeHtml(eventName(row.topic0))}</td>
        </tr>
      `,
    )
    .join("");

  return `
    <div class="table-wrap">
      <table class="data-table">
        <thead>
          <tr>
            <th>Block</th>
            <th>Time</th>
            <th>Relation</th>
            <th>Tx</th>
            <th>Contract</th>
            <th>Event</th>
          </tr>
        </thead>
        <tbody>${body}</tbody>
      </table>
    </div>
  `;
}

function renderContractPanel(address, kind, summary, rows, inspect, methodsPayload, verificationPayload) {
  if (!kind.isContract) {
    return `
      <div class="notice-card">
        <div class="notice-copy">
          This address does not currently have contract-like signals in the indexed data or live RPC bytecode reads.
        </div>
      </div>
    `;
  }

  const profile = inspect?.profile || null;
  const creator = inspect?.creator || null;
  const methods = methodsPayload?.methods || [];
  const verification = verificationPayload?.verification || null;
  const readFunctions = Array.isArray(verificationPayload?.read_functions) ? verificationPayload.read_functions : [];
  const label = verificationPayload?.label || inspect?.label || null;
  const readRows = [
    ["Name", profile?.name],
    ["Symbol", profile?.symbol],
    ["Decimals", profile?.decimals],
    ["Total supply", profile?.total_supply ? formatTokenAmount(profile.total_supply, profile?.decimals ?? 0, 6) : null],
    ["ERC165", profile?.supports_erc165 ? "Yes" : null],
    ["ERC721", profile?.supports_erc721 ? "Yes" : null],
    ["ERC1155", profile?.supports_erc1155 ? "Yes" : null],
  ].filter(([, value]) => value !== null && value !== undefined && value !== "");

  return `
    <div class="panel-stack">
      <div class="kpi-grid kpi-grid-tight">
        ${renderKpiCard("Created block", summary.created_block ? formatNumber(summary.created_block) : "-", "Contract deployment")}
        ${renderKpiCard("Emitted logs", formatNumber(summary.emitted_logs || 0), "Logs from this address")}
        ${renderKpiCard("Bytecode", profile?.is_contract ? `${formatNumber(profile.bytecode_size || 0)} B` : "-", "Latest eth_getCode snapshot")}
      </div>
      <section class="subpanel-card">
        <div class="subpanel-title">Verification</div>
        ${
          verification
            ? `
              <div class="definition-list">
                ${renderDefinitionItem("Contract", verification.summary.contract_name)}
                ${renderDefinitionItem("Status", formatVerificationStatus(verification.summary.verification_status))}
                ${renderDefinitionItem("Bytecode proof", formatBytecodeProof(verification.summary))}
                ${renderDefinitionItem("Compiler", verification.summary.compiler_version || "-")}
                ${renderDefinitionItem("Language", verification.summary.language || "-")}
                ${renderDefinitionItem("License", verification.summary.license || "-")}
                ${renderDefinitionItem("Label", label?.label || "-")}
                ${renderDefinitionItem("On-chain code hash", verification.summary.deployed_runtime_code_hash || "-")}
                ${renderDefinitionItem("Source imported", verification.summary.has_source_code ? "Yes" : "No")}
                ${renderDefinitionItem("Runtime bytecode submitted", verification.summary.has_runtime_bytecode ? "Yes" : "No")}
                ${renderDefinitionItem("Status note", verification.summary.status_reason || "-")}
                ${renderDefinitionItem("Verified at", formatTimestamp(verification.summary.verified_at))}
              </div>
            `
            : '<div class="empty-mini">No stored verification record yet. Use the explorer admin panel below to import ABI and source metadata for this contract.</div>'
        }
      </section>
      <div class="split-grid">
        <section class="subpanel-card">
          <div class="subpanel-title">Contract profile</div>
          <div class="definition-list">
            ${renderDefinitionItem("Kind", profile?.detected_kind ? profile.detected_kind.toUpperCase() : kind.badge)}
            ${renderDefinitionItem("Code hash", profile?.code_hash || "-")}
            ${renderDefinitionItem("Preview", profile?.code_preview || "-")}
            ${renderDefinitionItem("Creator", creator ? renderMonoLink(`/explore/address/${creator.creator_address}`, creator.creator_address, false) : '<span class="text-secondary">-</span>')}
            ${renderDefinitionItem("Creation tx", creator ? renderMonoLink(`/explore/receipt/${creator.tx_hash}`, creator.tx_hash, false) : '<span class="text-secondary">-</span>')}
          </div>
        </section>
        <section class="subpanel-card">
          <div class="subpanel-title">Read contract</div>
          ${
            verification && readFunctions.length
              ? renderReadContractPanel(address, readFunctions)
              : readRows.length
                ? `<div class="definition-list">${readRows.map(([labelName, value]) => renderDefinitionItem(labelName, value)).join("")}</div>`
                : '<div class="empty-mini">No stored ABI or standard ERC reads are available for this contract yet.</div>'
          }
        </section>
      </div>
      ${
        verification?.source_code
          ? `
            <section class="subpanel-card">
              <div class="subpanel-title">Source code</div>
              <pre class="code-block mono">${escapeHtml(verification.source_code)}</pre>
            </section>
          `
          : ""
      }
      <section class="subpanel-card">
        <div class="subpanel-title">Top methods</div>
        ${renderContractMethodsTable(methods, verificationPayload)}
      </section>
      <div class="table-wrap">
        <table class="data-table">
          <thead>
            <tr>
              <th>Block</th>
              <th>Time</th>
              <th>Tx</th>
              <th>Event</th>
            </tr>
          </thead>
          <tbody>
            ${
              rows.length
                ? rows
                    .map(
                      (row) => `
                        <tr>
                          <td class="mono"><a href="/explore/block/${row.block_num}">${formatNumber(row.block_num)}</a></td>
                          <td>${escapeHtml(formatRelativeTime(row.block_timestamp))}</td>
                          <td class="mono"><a href="/explore/receipt/${with0x(row.tx_hash)}">${escapeHtml(shortHex(with0x(row.tx_hash), 10))}</a></td>
                          <td>${escapeHtml(eventName(row.topic0))}</td>
                        </tr>
                      `,
                    )
                    .join("")
                : '<tr><td colspan="4" class="text-secondary">No emitted logs found.</td></tr>'
            }
          </tbody>
        </table>
      </div>
    </div>
  `;
}

function renderTokenPanel(kind, tokenSummary, inspect, holdersPayload, transfersPayload, approvalsPayload) {
  if (!kind.isToken) {
    return `
      <div class="notice-card">
        <div class="notice-copy">
          This address is not currently classified as token-like from indexed logs. The explorer only infers token
          contracts from Transfer and Approval event signatures right now.
        </div>
      </div>
    `;
  }

  const profile = inspect?.profile || holdersPayload?.profile || transfersPayload?.profile || approvalsPayload?.profile || null;
  const holders = holdersPayload?.holders || [];
  const transfers = transfersPayload?.transfers || [];
  const approvals = approvalsPayload?.approvals || [];
  const holderCount = Number(holdersPayload?.total_holders || 0);
  const label = inspect?.label?.label || null;

  return `
    <div class="panel-stack">
      <div class="kpi-grid kpi-grid-tight">
        ${renderKpiCard("Transfers", formatNumber(tokenSummary.transfer_count || 0), "Transfer event count")}
        ${renderKpiCard("Approvals", formatNumber(tokenSummary.approval_count || 0), "Approval event count")}
        ${renderKpiCard("Holders", formatNumber(holderCount), "Positive balances inferred from logs")}
      </div>
      <div class="split-grid">
        <section class="subpanel-card">
          <div class="subpanel-title">Token profile</div>
          <div class="definition-list">
            ${renderDefinitionItem("Label", label || "-")}
            ${renderDefinitionItem("Standard", profile?.detected_kind ? profile.detected_kind.toUpperCase() : "TOKEN")}
            ${renderDefinitionItem("Name", profile?.name || "-")}
            ${renderDefinitionItem("Symbol", profile?.symbol || "-")}
            ${renderDefinitionItem("Decimals", profile?.decimals ?? "-")}
            ${renderDefinitionItem("Total supply", profile?.total_supply ? formatTokenAmount(profile.total_supply, profile?.decimals ?? 0, 6) : "-")}
          </div>
        </section>
        <section class="subpanel-card">
          <div class="subpanel-title">Top holders</div>
          ${renderTokenHoldersTable(holders, profile?.decimals ?? 0)}
        </section>
      </div>
      <section class="subpanel-card">
        <div class="subpanel-title">Recent transfers</div>
        ${renderTokenTransfersTable(transfers, profile?.decimals ?? 0)}
      </section>
      <section class="subpanel-card">
        <div class="subpanel-title">Recent approvals</div>
        ${renderTokenApprovalsTable(approvals, profile?.decimals ?? 0)}
      </section>
    </div>
  `;
}

function renderPortfolioPanel(portfolio, profile) {
  if (!portfolio) {
    return `
      <div class="notice-card">
        <div class="notice-copy">
          Portfolio data is unavailable right now. The explorer needs live RPC access plus indexed Transfer logs to build this view.
        </div>
      </div>
    `;
  }

  return `
    <div class="panel-stack">
      <div class="kpi-grid kpi-grid-tight">
        ${renderKpiCard("Native balance", formatNativeAmount(portfolio.native_balance || profile?.native_balance || "0", 6), "Latest RPC balance")}
        ${renderKpiCard("Token holdings", formatNumber(portfolio.holdings?.length || 0), "Positive balances on this page")}
        ${renderKpiCard("Account kind", profile?.detected_kind ? profile.detected_kind.toUpperCase() : "ACCOUNT", "Live bytecode classification")}
      </div>
      ${renderPortfolioTable(portfolio.holdings || [])}
    </div>
  `;
}

function renderTokensTable(rows) {
  if (!rows.length) {
    return '<div class="empty-state">No token-like contracts inferred yet.</div>';
  }

  const body = rows
    .map((row) => {
      const title = row.label || row.symbol || row.name || shortHex(row.address, 8);
      const subtitleParts = [row.name, row.symbol].filter(Boolean);
      return `
        <tr>
          <td>
            <div class="table-primary-cell">
              <a href="/explore/token/${with0x(row.address)}">${escapeHtml(title)}</a>
              ${row.is_official ? '<span class="mini-badge">Official</span>' : ""}
            </div>
            <div class="table-secondary-cell mono">${escapeHtml(subtitleParts.join(" · ") || with0x(row.address))}</div>
          </td>
          <td>${escapeHtml((row.detected_kind || "token").toUpperCase())}</td>
          <td class="align-right mono">${row.total_supply ? formatTokenAmount(row.total_supply, row.decimals ?? 0, 4) : "-"}</td>
          <td class="align-right mono">${formatNumber(row.transfer_count || 0)}</td>
          <td class="align-right mono">${formatNumber(row.approval_count || 0)}</td>
          <td class="align-right mono"><a href="/explore/block/${row.last_seen_block}">${formatNumber(row.last_seen_block)}</a></td>
        </tr>
      `;
    })
    .join("");

  return `
    <table class="data-table">
      <thead>
        <tr>
          <th>Token</th>
          <th>Kind</th>
          <th class="align-right">Supply</th>
          <th class="align-right">Transfers</th>
          <th class="align-right">Approvals</th>
          <th class="align-right">Last seen</th>
        </tr>
      </thead>
      <tbody>${body}</tbody>
    </table>
  `;
}

function renderLogsTable(rows, decodedLogs = []) {
  if (!rows.length) {
    return '<div class="empty-state">No logs recorded for this receipt.</div>';
  }

  const decodedByIndex = new Map(
    (Array.isArray(decodedLogs) ? decodedLogs : []).map((entry) => [Number(entry.log_idx), entry]),
  );

  const body = rows
    .map((row) => {
      const decoded = decodedByIndex.get(Number(row.log_idx)) || null;
      const eventLabel = decoded?.label || eventName(row.topic0);
      const eventSignature = decoded?.signature || null;
      const decodedSummary = renderDecodedArgsPreview(decoded);
      return `
        <tr>
          <td class="mono">${formatNumber(row.log_idx)}</td>
          <td class="mono"><a href="/explore/address/${with0x(row.address)}?tab=contract">${escapeHtml(shortHex(with0x(row.address), 8))}</a></td>
          <td>
            <div class="table-primary-cell">${escapeHtml(eventLabel)}</div>
            <div class="table-secondary-cell mono">${escapeHtml(eventSignature || with0x(row.topic0) || "-")}</div>
          </td>
          <td>${decodedSummary}</td>
          <td>${renderDecodeSourceInline(decoded)}</td>
          <td class="align-right mono">${formatNumber(row.data_length || 0)}</td>
        </tr>
      `;
    })
    .join("");

  return `
    <div class="table-wrap">
      <table class="data-table">
        <thead>
          <tr>
            <th>Index</th>
            <th>Address</th>
            <th>Event</th>
            <th>Decoded</th>
            <th>Source</th>
            <th class="align-right">Data bytes</th>
          </tr>
        </thead>
        <tbody>${body}</tbody>
      </table>
    </div>
  `;
}

function renderSearchResultsTable(rows) {
  if (!rows.length) {
    return '<div class="empty-state">No explorer results matched that search.</div>';
  }

  const body = rows
    .map(
      (row) => `
        <tr>
          <td>${escapeHtml(row.entity_type || "-")}</td>
          <td><a href="${row.href}">${escapeHtml(row.title || "-")}</a></td>
          <td>${escapeHtml(row.subtitle || "-")}</td>
          <td class="mono">${escapeHtml(shortHex(row.address || row.tx_hash || "", 10))}</td>
        </tr>
      `,
    )
    .join("");

  return `
    <div class="table-wrap">
      <table class="data-table">
        <thead>
          <tr>
            <th>Type</th>
            <th>Title</th>
            <th>Subtitle</th>
            <th>Reference</th>
          </tr>
        </thead>
        <tbody>${body}</tbody>
      </table>
    </div>
  `;
}

function renderReadContractPanel(address, functions) {
  return `
    <div class="read-contract-stack">
      ${functions
        .slice(0, 24)
        .map(
          (fn, index) => `
            <section class="read-contract-card">
              <div class="read-contract-head">
                <div>
                  <div class="read-contract-title mono">${escapeHtml(fn.signature)}</div>
                  <div class="read-contract-subtitle">${escapeHtml(fn.state_mutability)} read via stored ABI</div>
                </div>
              </div>
              <form
                class="read-contract-form"
                data-read-contract-form
                data-address="${escapeHtml(address)}"
                data-selector="${escapeHtml(fn.selector)}"
              >
                ${fn.inputs
                  .map(
                    (input, inputIndex) => `
                      <label class="field-stack">
                        <span class="field-label">${escapeHtml(input.name || `arg${inputIndex}`)} · ${escapeHtml(input.kind)}</span>
                        <input class="search-input search-input-small" type="text" name="arg-${inputIndex}" placeholder="${escapeHtml(input.kind)}" />
                      </label>
                    `,
                  )
                  .join("")}
                <div class="read-contract-actions">
                  <button class="pagination-link" type="submit">Call</button>
                </div>
              </form>
              <div class="read-contract-result" id="read-contract-result-${index}"></div>
            </section>
          `,
        )
        .join("")}
    </div>
  `;
}

function renderInputPanel(inputData, decodedInput) {
  const rawInput = with0x(inputData);
  const decodedRows = decodedInput?.args || [];
  const sourceMeta = decodedInput?.decode_source
    ? `
      <section class="subpanel-card">
        <div class="subpanel-title">Decoder</div>
        <div class="definition-list">
          ${renderDefinitionItem("Source", escapeHtml(humanizeDecodeSource(decodedInput.decode_source)))}
          ${renderDefinitionItem("Confidence", escapeHtml(decodedInput.confidence || "-"))}
          ${renderDefinitionItem("Family", escapeHtml(decodedInput.protocol_family || "-"))}
          ${renderDefinitionItem("Pack", escapeHtml(decodedInput.protocol_pack || "-"))}
          ${
            Array.isArray(decodedInput.alternatives) && decodedInput.alternatives.length
              ? renderDefinitionItem("Alternatives", escapeHtml(decodedInput.alternatives.join(" · ")))
              : ""
          }
          ${
            Array.isArray(decodedInput.notes) && decodedInput.notes.length
              ? renderDefinitionItem("Notes", escapeHtml(decodedInput.notes.join(" ")))
              : ""
          }
        </div>
      </section>
    `
    : "";

  return `
    <div class="panel-stack panel-stack-tight">
      ${sourceMeta}
      ${
        decodedRows.length
          ? `
            <section class="subpanel-card">
              <div class="subpanel-title">Decoded arguments</div>
              <div class="definition-list">
                ${decodedRows
                  .map((arg, index) => {
                    const label = arg.name || arg.label || `arg${index}`;
                    const kind = arg.kind || arg.type || "unknown";
                    const value = arg.href
                      ? renderMonoLink(arg.href, arg.display || arg.value, false)
                      : escapeHtml(arg.display || arg.value || "-");
                    const note = arg.note ? `<div class="table-secondary-cell">${escapeHtml(arg.note)}</div>` : "";
                    return renderDefinitionItem(
                      `${label} · ${kind}${arg.indexed ? " · indexed" : ""}`,
                      `<span>${value}</span>${note}`,
                    );
                  })
                  .join("")}
              </div>
            </section>
          `
          : `
            <section class="subpanel-card">
              <div class="subpanel-title">Decoded arguments</div>
              <div class="empty-mini">No built-in decoder matched this calldata. Raw input is shown below.</div>
            </section>
          `
      }
      <section class="subpanel-card">
        <div class="subpanel-title">Raw calldata</div>
        <pre class="code-block mono">${escapeHtml(rawInput || "0x")}</pre>
      </section>
    </div>
  `;
}

function renderContractMethodsTable(rows, verificationPayload) {
  if (!rows.length) {
    return '<div class="empty-mini">No method selectors have been indexed for this contract yet.</div>';
  }

  const body = rows
    .map(
      (row) => `
        <tr>
          <td>
            <div class="table-primary-cell mono">${escapeHtml(row.method_label || resolveMethodLabel(row.selector, verificationPayload) || "Unknown")}</div>
            <div class="table-secondary-cell">${escapeHtml(humanizeDecodeSource(row.decode_source || "unknown"))}${row.protocol_pack ? ` · ${escapeHtml(row.protocol_pack)}` : ""}</div>
          </td>
          <td class="mono">${escapeHtml(row.selector)}</td>
          <td class="align-right mono">${formatNumber(row.call_count || 0)}</td>
          <td class="align-right mono">${formatNumber(row.success_count || 0)}</td>
          <td class="align-right mono"><a href="/explore/block/${row.last_block}">${formatNumber(row.last_block)}</a></td>
        </tr>
      `,
    )
    .join("");

  return `
    <div class="table-wrap">
      <table class="data-table">
        <thead>
          <tr>
            <th>Method</th>
            <th>Selector</th>
            <th class="align-right">Calls</th>
            <th class="align-right">Success</th>
            <th class="align-right">Last block</th>
          </tr>
        </thead>
        <tbody>${body}</tbody>
      </table>
    </div>
  `;
}

function renderTokenHoldersTable(rows, decimals) {
  if (!rows.length) {
    return '<div class="empty-mini">No positive holders inferred from indexed transfers yet.</div>';
  }

  const body = rows
    .map(
      (row) => `
        <tr>
          <td class="mono"><a href="/explore/address/${row.holder_address}">${escapeHtml(shortHex(row.holder_address, 8))}</a></td>
          <td class="align-right mono">${formatTokenAmount(row.balance, decimals, 6)}</td>
        </tr>
      `,
    )
    .join("");

  return `
    <div class="table-wrap">
      <table class="data-table">
        <thead>
          <tr>
            <th>Holder</th>
            <th class="align-right">Balance</th>
          </tr>
        </thead>
        <tbody>${body}</tbody>
      </table>
    </div>
  `;
}

function renderTokenTransfersTable(rows, decimals) {
  if (!rows.length) {
    return '<div class="empty-mini">No transfers indexed for this token yet.</div>';
  }

  const body = rows
    .map(
      (row) => `
        <tr>
          <td class="mono"><a href="/explore/block/${row.block_num}">${formatNumber(row.block_num)}</a></td>
          <td>${escapeHtml(formatRelativeTime(row.block_timestamp))}</td>
          <td class="mono"><a href="/explore/address/${row.from_address}">${escapeHtml(shortHex(row.from_address, 8))}</a></td>
          <td class="mono"><a href="/explore/address/${row.to_address}">${escapeHtml(shortHex(row.to_address, 8))}</a></td>
          <td class="align-right mono">${formatTokenAmount(row.amount, decimals, 6)}</td>
          <td class="mono"><a href="/explore/receipt/${row.tx_hash}">${escapeHtml(shortHex(row.tx_hash, 10))}</a></td>
        </tr>
      `,
    )
    .join("");

  return `
    <div class="table-wrap">
      <table class="data-table">
        <thead>
          <tr>
            <th>Block</th>
            <th>Time</th>
            <th>From</th>
            <th>To</th>
            <th class="align-right">Amount</th>
            <th>Tx</th>
          </tr>
        </thead>
        <tbody>${body}</tbody>
      </table>
    </div>
  `;
}

function renderTokenApprovalsTable(rows, decimals) {
  if (!rows.length) {
    return '<div class="empty-mini">No approval events indexed for this token yet.</div>';
  }

  const body = rows
    .map(
      (row) => `
        <tr>
          <td class="mono"><a href="/explore/block/${row.block_num}">${formatNumber(row.block_num)}</a></td>
          <td>${escapeHtml(formatRelativeTime(row.block_timestamp))}</td>
          <td class="mono"><a href="/explore/address/${row.owner_address}">${escapeHtml(shortHex(row.owner_address, 8))}</a></td>
          <td class="mono"><a href="/explore/address/${row.spender_address}">${escapeHtml(shortHex(row.spender_address, 8))}</a></td>
          <td class="align-right mono">${formatTokenAmount(row.amount, decimals, 6)}</td>
          <td class="mono"><a href="/explore/receipt/${row.tx_hash}">${escapeHtml(shortHex(row.tx_hash, 10))}</a></td>
        </tr>
      `,
    )
    .join("");

  return `
    <div class="table-wrap">
      <table class="data-table">
        <thead>
          <tr>
            <th>Block</th>
            <th>Time</th>
            <th>Owner</th>
            <th>Spender</th>
            <th class="align-right">Value / ID</th>
            <th>Tx</th>
          </tr>
        </thead>
        <tbody>${body}</tbody>
      </table>
    </div>
  `;
}

function renderPortfolioTable(rows) {
  if (!rows.length) {
    return '<div class="empty-state">No positive token balances inferred for this address yet.</div>';
  }

  const body = rows
    .map((row) => {
      const metadata = row.metadata || {};
      const label = metadata.symbol || metadata.name || shortHex(row.token_address, 8);
      return `
        <tr>
          <td class="mono"><a href="/explore/token/${row.token_address}">${escapeHtml(label)}</a></td>
          <td>${escapeHtml(metadata.name || metadata.detected_kind?.toUpperCase?.() || "Token")}</td>
          <td class="align-right mono">${formatTokenAmount(row.balance, metadata.decimals ?? 0, 6)}</td>
          <td class="align-right mono">${formatNumber(row.received_count || 0)}</td>
          <td class="align-right mono"><a href="/explore/block/${row.last_block}">${formatNumber(row.last_block)}</a></td>
        </tr>
      `;
    })
    .join("");

  return `
    <div class="table-wrap">
      <table class="data-table">
        <thead>
          <tr>
            <th>Token</th>
            <th>Profile</th>
            <th class="align-right">Balance</th>
            <th class="align-right">Inbound</th>
            <th class="align-right">Last block</th>
          </tr>
        </thead>
        <tbody>${body}</tbody>
      </table>
    </div>
  `;
}

function renderContractsTable(rows) {
  if (!rows.length) {
    return '<div class="empty-state">No verified contracts have been imported yet.</div>';
  }

  const body = rows
    .map((row) => {
      const href = row.detected_kind === "token"
        ? `/explore/token/${row.address}`
        : `/explore/address/${row.address}?tab=contract`;
      const primary = row.symbol || row.name || row.label || row.contract_name || shortHex(row.address, 8);
      const profile = row.label || (row.detected_kind === "token" ? "Token" : "Contract");
      const proof = row.bytecode_match === true ? "Matched" : row.bytecode_match === false ? "Mismatch" : "Metadata";

      return `
        <tr>
          <td>
            <div class="table-primary-cell"><a href="${href}">${escapeHtml(primary)}</a></div>
            <div class="table-secondary-cell mono">${escapeHtml(shortHex(row.address, 8))}</div>
          </td>
          <td>${escapeHtml(row.contract_name || "-")}</td>
          <td>${escapeHtml(profile)}</td>
          <td>${escapeHtml(formatVerificationStatus(row.verification_status))}</td>
          <td>${escapeHtml(proof)}</td>
          <td>${escapeHtml(row.compiler_version || "-")}</td>
          <td>${row.has_source_code ? "Yes" : "No"}</td>
          <td>${escapeHtml(formatRelativeTime(row.verified_at))}</td>
        </tr>
      `;
    })
    .join("");

  return `
    <div class="table-wrap">
      <table class="data-table">
        <thead>
          <tr>
            <th>Address</th>
            <th>Contract</th>
            <th>Profile</th>
            <th>Status</th>
            <th>Proof</th>
            <th>Compiler</th>
            <th>Source</th>
            <th>Imported</th>
          </tr>
        </thead>
        <tbody>${body}</tbody>
      </table>
    </div>
  `;
}

function renderAddressTabs(address, activeTab, kind) {
  const tabs = [
    { key: "transactions", label: "Transactions" },
    { key: "portfolio", label: "Portfolio" },
    { key: "logs", label: "Logs" },
  ];

  if (kind.isContract) {
    tabs.push({ key: "contract", label: "Contract" });
  }
  if (kind.isToken) {
    tabs.push({ key: "token", label: "Token" });
  }

  return tabs
    .map(({ key, label }) => {
      const isActive = key === activeTab;
      const href = buildUrl(`/explore/address/${address}`, { tab: key });
      return `<a class="tab-link ${isActive ? "tab-link-active" : ""}" href="${href}">${escapeHtml(label)}</a>`;
    })
    .join("");
}

function renderSearchForm({ compact, autofocus = false }) {
  const inputClass = compact ? "search-input search-input-small" : "search-input search-input-large";
  const autoFocusAttr = autofocus ? "autofocus" : "";
  return `
    <form class="search-form" data-search-form>
      <input
        ${autoFocusAttr}
        class="${inputClass}"
        type="text"
        name="explore-query"
        placeholder="Search by Address / Tx Hash / Block / Token"
        spellcheck="false"
        autocomplete="off"
      />
      <button class="search-submit" type="submit" aria-label="Search">
        ${icons.arrow}
      </button>
    </form>
  `;
}

function bindSearchForms() {
  document.querySelectorAll("[data-search-form]").forEach((form) => {
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const input = form.querySelector("input[name='explore-query']");
      const value = input?.value?.trim() || "";
      const target = await resolveSearchTarget(value);
      if (!target) {
        input?.focus();
        return;
      }
      window.location.href = target;
    });
  });
}

async function resolveSearchTarget(value) {
  if (!value) {
    return null;
  }

  if (/^\d+$/.test(value)) {
    return `/explore/block/${value}`;
  }

  const normalized = normalizeHex(value);
  if (isHex(normalized, 64)) {
    return `/explore/receipt/${normalized}`;
  }
  if (isHex(normalized, 40)) {
    return `/explore/address/${normalized}`;
  }

  const payload = await tryFetchExplorerJson(
    `/explore/api/search?chainId=${encodeURIComponent(state.chainId)}&q=${encodeURIComponent(value)}`,
  );
  const results = Array.isArray(payload?.results) ? payload.results : [];
  if (results.length === 1 && results[0]?.href) {
    return results[0].href;
  }

  return buildUrl("/explore/search", { q: value });
}

function renderPillLink(label, href, icon) {
  return `
    <a class="pill-link" href="${href}">
      <span class="pill-icon">${icon}</span>
      <span>${escapeHtml(label)}</span>
    </a>
  `;
}

function renderSummaryRow(label, value) {
  return `
    <div class="summary-row">
      <span class="summary-label">${escapeHtml(label)}</span>
      <span class="summary-value mono">${value}</span>
    </div>
  `;
}

function renderDefinitionItem(label, value) {
  const renderedValue =
    typeof value === "string" && value.trim().startsWith("<") ? value : escapeHtml(String(value ?? "-"));
  return `
    <div class="definition-item">
      <div class="definition-label">${escapeHtml(label)}</div>
      <div class="definition-value mono">${renderedValue}</div>
    </div>
  `;
}

function renderExplorerAdminPanel(address, kind, inspect, verificationPayload) {
  const label = inspect?.label || {};
  const verification = verificationPayload?.verification || null;
  const summary = verification?.summary || inspect?.verification || {};
  const abiJson = verification?.abi ? JSON.stringify(verification.abi, null, 2) : "";
  const sourceCode = verification?.source_code || "";
  const runtimeBytecode = verification?.submitted_runtime_bytecode || "";

  return `
    <section class="panel-card explorer-admin-panel">
      <div class="panel-header">
        <div>
          <div class="panel-title">Explorer admin</div>
          <div class="panel-subtitle">Trusted local metadata tools for labels, verification imports, and cache refresh</div>
        </div>
      </div>
      <div class="panel-body">
        <div class="panel-stack panel-stack-tight">
          <div class="split-grid">
            <section class="subpanel-card">
              <div class="subpanel-title">Address label</div>
              <form class="explorer-admin-form" data-label-form data-address="${escapeHtml(address)}">
                <label class="field-stack">
                  <span class="field-label">Label</span>
                  <input class="search-input search-input-small" type="text" name="label" value="${escapeHtml(label.label || "")}" placeholder="Treasury, Bridge, Token, Router" />
                </label>
                <label class="field-stack">
                  <span class="field-label">Category</span>
                  <input class="search-input search-input-small" type="text" name="category" value="${escapeHtml(label.category || "")}" placeholder="token" />
                </label>
                <label class="field-stack">
                  <span class="field-label">Website</span>
                  <input class="search-input search-input-small" type="text" name="website" value="${escapeHtml(label.website || "")}" placeholder="https://igralabs.com" />
                </label>
                <label class="field-stack">
                  <span class="field-label">Notes</span>
                  <textarea class="text-area-input mono" name="notes" rows="4" placeholder="Optional internal note">${escapeHtml(label.notes || "")}</textarea>
                </label>
                <label class="checkbox-row">
                  <input type="checkbox" name="is_official" ${label.is_official ? "checked" : ""} />
                  <span>Official label</span>
                </label>
                <div class="admin-action-row">
                  <button class="pagination-link" type="submit">Save label</button>
                  <span class="admin-status" data-label-status></span>
                </div>
              </form>
            </section>

            <section class="subpanel-card">
              <div class="subpanel-title">Metadata cache</div>
              <div class="empty-mini">
                Refresh live RPC metadata and token cache for this address without restarting the indexer.
              </div>
              <div class="admin-action-row admin-action-row-pad">
                <button class="pagination-link" type="button" data-refresh-address data-address="${escapeHtml(address)}">
                  Refresh metadata
                </button>
                <span class="admin-status" data-refresh-status></span>
              </div>
            </section>
          </div>

          ${
            kind.isContract
              ? `
                <section class="subpanel-card">
                  <div class="subpanel-title">Contract verification import</div>
                  <form class="explorer-admin-form" data-verify-form data-address="${escapeHtml(address)}">
                    <div class="split-grid">
                      <label class="field-stack">
                        <span class="field-label">Contract name</span>
                        <input class="search-input search-input-small" type="text" name="contract_name" value="${escapeHtml(summary.contract_name || inspect?.profile?.name || label.label || "")}" placeholder="HeartbeatToken0" />
                      </label>
                      <label class="field-stack">
                        <span class="field-label">Language</span>
                        <input class="search-input search-input-small" type="text" name="language" value="${escapeHtml(summary.language || "Solidity")}" placeholder="Solidity" />
                      </label>
                      <label class="field-stack">
                        <span class="field-label">Compiler version</span>
                        <input class="search-input search-input-small" type="text" name="compiler_version" value="${escapeHtml(summary.compiler_version || "")}" placeholder="v0.8.x+commit..." />
                      </label>
                      <label class="field-stack">
                        <span class="field-label">License</span>
                        <input class="search-input search-input-small" type="text" name="license" value="${escapeHtml(summary.license || "")}" placeholder="MIT" />
                      </label>
                      <label class="field-stack">
                        <span class="field-label">Optimization runs</span>
                        <input class="search-input search-input-small" type="number" name="optimization_runs" value="${escapeHtml(summary.optimization_runs ?? "")}" placeholder="200" />
                      </label>
                      <label class="checkbox-row checkbox-row-inline">
                        <input type="checkbox" name="optimization_enabled" ${summary.optimization_enabled ? "checked" : ""} />
                        <span>Optimizer enabled</span>
                      </label>
                    </div>
                    <label class="field-stack">
                      <span class="field-label">Runtime bytecode</span>
                      <textarea class="text-area-input mono" name="runtime_bytecode" rows="6" placeholder="Paste deployed/runtime bytecode from your artifact to prove a bytecode match">${escapeHtml(runtimeBytecode)}</textarea>
                    </label>
                    <label class="field-stack">
                      <span class="field-label">ABI JSON</span>
                      <textarea class="text-area-input mono text-area-large" name="abi" rows="12" placeholder='[{"type":"function","name":"name",...}]'>${escapeHtml(abiJson)}</textarea>
                    </label>
                    <label class="field-stack">
                      <span class="field-label">Source code</span>
                      <textarea class="text-area-input mono text-area-large" name="source_code" rows="12" placeholder="Optional verified source code">${escapeHtml(sourceCode)}</textarea>
                    </label>
                    <div class="admin-action-row">
                      <button class="pagination-link" type="submit">Save verification</button>
                      <span class="admin-status" data-verify-status></span>
                    </div>
                  </form>
                </section>
              `
              : ""
          }
        </div>
      </div>
    </section>
  `;
}

function bindExplorerAdminForms() {
  document.querySelectorAll("[data-label-form]").forEach((form) => {
    if (form.dataset.bound === "true") {
      return;
    }
    form.dataset.bound = "true";
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const address = form.dataset.address;
      const status = form.querySelector("[data-label-status]");
      const labelField = form.querySelector("[name='label']");
      const categoryField = form.querySelector("[name='category']");
      const websiteField = form.querySelector("[name='website']");
      const notesField = form.querySelector("[name='notes']");
      const officialField = form.querySelector("[name='is_official']");
      const payload = {
        label: labelField?.value.trim() || "",
        category: optionalFieldValue(categoryField?.value),
        website: optionalFieldValue(websiteField?.value),
        notes: optionalFieldValue(notesField?.value),
        is_official: Boolean(officialField?.checked),
      };

      setAdminStatus(status, "Saving label…");
      try {
        await fetchJson(`/explore/api/labels/${address}?chainId=${encodeURIComponent(state.chainId)}`, {
          method: "PUT",
          headers: {
            "Content-Type": "application/json",
          },
          body: JSON.stringify(payload),
        });
        setAdminStatus(status, "Label saved");
        await renderRoute();
      } catch (error) {
        setAdminStatus(status, error.message || "Label save failed", true);
      }
    });
  });

  document.querySelectorAll("[data-verify-form]").forEach((form) => {
    if (form.dataset.bound === "true") {
      return;
    }
    form.dataset.bound = "true";
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const address = form.dataset.address;
      const status = form.querySelector("[data-verify-status]");
      const contractNameField = form.querySelector("[name='contract_name']");
      const abiField = form.querySelector("[name='abi']");
      const runtimeBytecodeField = form.querySelector("[name='runtime_bytecode']");
      const sourceCodeField = form.querySelector("[name='source_code']");
      const languageField = form.querySelector("[name='language']");
      const compilerField = form.querySelector("[name='compiler_version']");
      const optimizationEnabledField = form.querySelector("[name='optimization_enabled']");
      const optimizationRunsField = form.querySelector("[name='optimization_runs']");
      const licenseField = form.querySelector("[name='license']");
      let abi;
      try {
        abi = JSON.parse(abiField?.value.trim() || "");
      } catch (_error) {
        setAdminStatus(status, "ABI must be valid JSON", true);
        return;
      }

      const payload = {
        contract_name: contractNameField?.value.trim() || "",
        abi,
        runtime_bytecode: optionalFieldValue(runtimeBytecodeField?.value),
        source_code: optionalFieldValue(sourceCodeField?.value),
        language: optionalFieldValue(languageField?.value),
        compiler_version: optionalFieldValue(compilerField?.value),
        optimization_enabled: Boolean(optimizationEnabledField?.checked),
        optimization_runs: optionalNumberValue(optimizationRunsField?.value),
        license: optionalFieldValue(licenseField?.value),
      };

      setAdminStatus(status, "Saving verification…");
      try {
        await fetchJson(`/explore/api/contract/${address}/verify?chainId=${encodeURIComponent(state.chainId)}`, {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
          },
          body: JSON.stringify(payload),
        });
        setAdminStatus(status, "Verification saved");
        await renderRoute();
      } catch (error) {
        setAdminStatus(status, error.message || "Verification save failed", true);
      }
    });
  });

  document.querySelectorAll("[data-refresh-address]").forEach((button) => {
    if (button.dataset.bound === "true") {
      return;
    }
    button.dataset.bound = "true";
    button.addEventListener("click", async () => {
      const address = button.dataset.address;
      const status = button.parentElement?.querySelector("[data-refresh-status]");
      setAdminStatus(status, "Refreshing…");
      try {
        await fetchJson(`/explore/api/address/${address}/refresh?chainId=${encodeURIComponent(state.chainId)}`, {
          method: "POST",
        });
        setAdminStatus(status, "Metadata refreshed");
        await renderRoute();
      } catch (error) {
        setAdminStatus(status, error.message || "Refresh failed", true);
      }
    });
  });
}

function setAdminStatus(node, message, isError = false) {
  if (!node) {
    return;
  }
  node.textContent = message || "";
  node.classList.toggle("admin-status-error", Boolean(isError));
}

function bindReadContractForms() {
  document.querySelectorAll("[data-read-contract-form]").forEach((form, formIndex) => {
    if (form.dataset.bound === "true") {
      return;
    }
    form.dataset.bound = "true";
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const address = form.dataset.address;
      const selector = form.dataset.selector;
      const result = form.parentElement?.querySelector(".read-contract-result");
      const args = Array.from(form.querySelectorAll("input")).map((input) => input.value.trim()).filter((value) => value.length > 0);

      if (result) {
        result.innerHTML = '<div class="empty-mini">Calling contract…</div>';
      }

      try {
        const payload = await fetchJson("/explore/api/contract/" + address + "/read?chainId=" + encodeURIComponent(state.chainId), {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
          },
          body: JSON.stringify({ selector, args }),
        });

        if (result) {
          result.innerHTML = renderReadContractResult(payload.outputs || []);
        }
      } catch (error) {
        if (result) {
          result.innerHTML = `<div class="empty-mini">${escapeHtml(error.message || "Contract call failed")}</div>`;
        }
      }
    });
  });
}

function renderReadContractResult(outputs) {
  if (!outputs.length) {
    return '<div class="empty-mini">No output values were returned.</div>';
  }

  return `
    <div class="definition-list">
      ${outputs.map((output, index) => renderDefinitionItem(`output${index} · ${output.kind}`, output.value)).join("")}
    </div>
  `;
}

function formatVerificationStatus(status) {
  switch (String(status || "").toLowerCase()) {
    case "fully_verified":
      return "Fully verified";
    case "bytecode_matched":
      return "Bytecode matched";
    case "imported":
      return "Imported";
    default:
      return '<span class="text-secondary">Unverified</span>';
  }
}

function formatBytecodeProof(summary) {
  if (!summary) {
    return '<span class="text-secondary">Not checked</span>';
  }
  if (summary.bytecode_match === true) {
    if (summary.bytecode_match_type === "metadata_stripped") {
      return "Metadata-stripped match";
    }
    return "Exact match";
  }
  if (summary.bytecode_match === false) {
    return "Mismatch";
  }
  return '<span class="text-secondary">Not checked</span>';
}

function renderKpiCard(label, value, note) {
  return `
    <div class="kpi-card">
      <div class="kpi-label">${escapeHtml(label)}</div>
      <div class="kpi-value mono">${escapeHtml(String(value))}</div>
      <div class="kpi-note">${escapeHtml(note)}</div>
    </div>
  `;
}

function renderPagination(path, page, hasNextPage, extraParams = {}) {
  const prevHref = page > 1 ? buildUrl(path, { ...extraParams, page: page - 1 }) : null;
  const nextHref = hasNextPage ? buildUrl(path, { ...extraParams, page: page + 1 }) : null;

  return `
    <div class="pagination">
      <div class="pagination-label">Page ${formatNumber(page)}</div>
      <div class="pagination-actions">
        ${
          prevHref
            ? `<a class="pagination-link" href="${prevHref}">Previous</a>`
            : '<span class="pagination-link pagination-link-disabled">Previous</span>'
        }
        ${
          nextHref
            ? `<a class="pagination-link" href="${nextHref}">Next</a>`
            : '<span class="pagination-link pagination-link-disabled">Next</span>'
        }
      </div>
    </div>
  `;
}

function renderMonoLink(href, value, short = true) {
  const display = short ? shortHex(value, 10) : value;
  return `<a href="${href}" class="wrap-anywhere">${escapeHtml(display)}</a>`;
}

function renderTxStatusBadge(kind) {
  const label =
    kind === "success" ? "Success" : kind === "failed" ? "Failed" : kind === "pending" ? "Pending" : "Unknown";
  return `<span class="tx-status tx-status-${kind}">${escapeHtml(label)}</span>`;
}

function statusKindFromValue(value) {
  if (value === null || value === undefined) {
    return "pending";
  }
  return Number(value) === 1 ? "success" : "failed";
}

function classifyAddress(summary, profile = null) {
  const liveKind = String(profile?.detected_kind || "").toLowerCase();
  const isContract =
    Boolean(profile?.is_contract) || Boolean(summary.was_created) || Number(summary.emitted_logs || 0) > 0;
  const isToken =
    liveKind === "erc20" ||
    liveKind === "erc721" ||
    liveKind === "erc1155" ||
    (isContract && (Number(summary.transfer_logs || 0) > 0 || Number(summary.approval_logs || 0) > 0));

  if (isToken) {
    const label =
      liveKind === "erc721" ? "ERC-721 Contract" : liveKind === "erc1155" ? "ERC-1155 Contract" : "Token Contract";
    return { isContract: true, isToken: true, badge: label, title: label };
  }
  if (isContract) {
    return { isContract: true, isToken: false, badge: "Contract", title: "Contract" };
  }
  return { isContract: false, isToken: false, badge: "Account", title: "Address" };
}

function normalizeAddressTab(requestedTab, kind) {
  const raw = String(requestedTab || "transactions").toLowerCase();
  if (raw === "index") {
    if (kind.isToken) {
      return "token";
    }
    if (kind.isContract) {
      return "contract";
    }
    return "transactions";
  }

  const allowed = new Set(["transactions", "portfolio", "logs"]);
  if (kind.isContract) {
    allowed.add("contract");
  }
  if (kind.isToken) {
    allowed.add("token");
  }

  return allowed.has(raw) ? raw : "transactions";
}

function topicToAddress(topic) {
  const normalized = with0x(topic);
  if (!/^0x[0-9a-f]{64}$/.test(normalized)) {
    return null;
  }
  return `0x${normalized.slice(-40)}`;
}

function eventName(topic0) {
  const normalized = with0x(topic0);
  return KNOWN_EVENTS[normalized] || shortHex(normalized, 10);
}

function methodName(selector) {
  const normalized = with0x(selector);
  if (!normalized) {
    return "No selector";
  }
  return KNOWN_METHODS[normalized]?.label || "Unknown";
}

function humanizeDecodeSource(value) {
  switch (String(value || "")) {
    case "verified_abi":
      return "Verified ABI";
    case "signature_registry":
      return "Signature registry";
    default:
      return "Unknown";
  }
}

function renderDecodedArgsPreview(decoded) {
  if (!decoded) {
    return '<span class="text-secondary">No decoder</span>';
  }

  const args = Array.isArray(decoded.args) ? decoded.args : [];
  if (!args.length) {
    return '<span class="text-secondary">No decoded args</span>';
  }

  const preview = args
    .slice(0, 2)
    .map((arg, index) => {
      const label = arg.name || arg.label || `arg${index}`;
      return `${label}=${arg.display || arg.value || "-"}`;
    })
    .join(" · ");
  const suffix = args.length > 2 ? ` +${args.length - 2} more` : "";
  return `<span class="mono wrap-anywhere">${escapeHtml(`${preview}${suffix}`)}</span>`;
}

function renderDecodeSourceInline(decoded) {
  if (!decoded) {
    return '<span class="text-secondary">Unknown</span>';
  }

  const parts = [humanizeDecodeSource(decoded.decode_source)];
  if (decoded.protocol_family) {
    parts.push(decoded.protocol_family);
  }
  if (decoded.protocol_pack) {
    parts.push(decoded.protocol_pack);
  }
  return escapeHtml(parts.join(" · "));
}

function selectorSignature(selector, verificationPayload) {
  const normalized = with0x(selector);
  if (!normalized) {
    return null;
  }

  const functions = Array.isArray(verificationPayload?.functions) ? verificationPayload.functions : [];
  const match = functions.find((fn) => with0x(fn.selector) === normalized);
  return match?.signature || null;
}

function resolveMethodLabel(selector, verificationPayload, decodedInput) {
  const normalized = with0x(selector);
  if (!normalized) {
    return null;
  }

  return selectorSignature(normalized, verificationPayload)
    || decodedInput?.label
    || KNOWN_METHODS[normalized]?.label
    || "Unknown";
}

function setDocumentTitle(title) {
  document.title = title;
}

async function runQuery(sql) {
  if (!state.chainId) {
    throw new Error("No chain ID is configured.");
  }

  const url = `/query?chainId=${encodeURIComponent(state.chainId)}&sql=${encodeURIComponent(compactSql(sql))}`;
  const response = await fetch(url);
  const payload = await safeJson(response);

  if (!response.ok || !payload?.ok) {
    const error = payload?.error || payload?.message || `Query failed (${response.status})`;
    throw new Error(error);
  }

  return rowsToObjects(payload.columns || [], payload.rows || []);
}

async function fetchExplorerJson(path) {
  return fetchJson(path);
}

async function fetchJson(path, options) {
  const response = await fetch(path, options);
  const payload = await safeJson(response);

  if (!response.ok || !payload?.ok) {
    const error = payload?.error || payload?.message || `Request failed (${response.status})`;
    throw new Error(error);
  }

  return payload;
}

async function tryFetchExplorerJson(path) {
  try {
    return await fetchExplorerJson(path);
  } catch (_error) {
    return null;
  }
}

async function safeJson(response) {
  try {
    return await response.json();
  } catch (_error) {
    return null;
  }
}

function rowsToObjects(columns, rows) {
  return rows.map((row) => {
    const record = {};
    columns.forEach((column, index) => {
      record[column] = row[index];
    });
    return record;
  });
}

function compactSql(sql) {
  return sql.replace(/\s+/g, " ").trim();
}

function normalizeHex(value) {
  const sanitized = sanitizeHexInput(value);
  if (!sanitized) {
    return "";
  }
  return sanitized.startsWith("0x") ? sanitized.toLowerCase() : `0x${sanitized.toLowerCase()}`;
}

function with0x(value) {
  const sanitized = sanitizeHexInput(value);
  if (!sanitized) {
    return "";
  }
  return sanitized.startsWith("0x") ? sanitized.toLowerCase() : `0x${sanitized.toLowerCase()}`;
}

function sanitizeHexInput(value) {
  let input = String(value || "").trim().replace(/^["']+|["']+$/g, "");
  for (let i = 0; i < 2; i += 1) {
    try {
      const decoded = decodeURIComponent(input);
      if (decoded === input) {
        break;
      }
      input = decoded;
    } catch {
      break;
    }
  }

  input = input.normalize("NFKC").replace(/[\u200B-\u200D\uFEFF]/g, "").trim();

  if (!input) {
    return "";
  }

  const hasPrefix = /^0x/i.test(input);
  const body = (hasPrefix ? input.slice(2) : input).replace(/[^0-9a-fA-F]/g, "");
  return hasPrefix ? `0x${body}` : body;
}

function isHex(value, bytes) {
  const normalized = normalizeHex(value);
  return new RegExp(`^0x[0-9a-f]{${bytes}}$`).test(normalized);
}

function hexBody(value) {
  return normalizeHex(value).slice(2);
}

function paddedAddressTopic(address) {
  return hexBody(address).padStart(64, "0");
}

function shortHex(value, visible = 8) {
  const normalized = with0x(value);
  if (!normalized) {
    return "-";
  }
  if (normalized.length <= visible * 2 + 2) {
    return normalized;
  }
  return `${normalized.slice(0, visible + 2)}...${normalized.slice(-visible)}`;
}

function formatNumber(value) {
  const num = Number(value);
  if (!Number.isFinite(num)) {
    return String(value ?? "-");
  }
  return new Intl.NumberFormat("en-US").format(num);
}

function formatChainId(value) {
  if (value === null || value === undefined || value === "") {
    return "-";
  }

  const raw = String(value).trim();
  return /^\d+$/.test(raw) ? raw : formatNumber(raw);
}

function formatNumericString(value) {
  if (value === null || value === undefined || value === "") {
    return "-";
  }
  const str = String(value);
  if (!/^-?\d+$/.test(str)) {
    return str;
  }

  const negative = str.startsWith("-");
  const digits = negative ? str.slice(1) : str;
  const parts = [];
  for (let index = digits.length; index > 0; index -= 3) {
    parts.unshift(digits.slice(Math.max(0, index - 3), index));
  }
  return `${negative ? "-" : ""}${parts.join(",")}`;
}

function formatNativeAmount(value, precision = 6) {
  const formatted = formatTokenAmount(value, 18, precision);
  return formatted === "-" ? formatted : `${formatted} ${NATIVE_SYMBOL}`;
}

function formatGasPrice(value, precision = 4) {
  const formatted = formatTokenAmount(value, 9, precision);
  return formatted === "-" ? formatted : `${formatted} Gwei`;
}

function formatGasUnits(value) {
  const formatted = formatNumber(value);
  return formatted === "-" ? formatted : `${formatted} gas`;
}

function multiplyNumericStrings(left, right) {
  try {
    if (left === null || right === null || left === undefined || right === undefined) {
      return null;
    }
    return (BigInt(String(left)) * BigInt(String(right))).toString();
  } catch (_error) {
    return null;
  }
}

function formatTokenAmount(value, decimals = 0, precision = 4) {
  if (value === null || value === undefined || value === "") {
    return "-";
  }

  const raw = String(value);
  if (!/^-?\d+$/.test(raw)) {
    return raw;
  }

  try {
    const negative = raw.startsWith("-");
    const digits = negative ? raw.slice(1) : raw;
    const places = Number.isFinite(Number(decimals)) ? Number(decimals) : 0;

    if (places <= 0) {
      return `${negative ? "-" : ""}${formatNumericString(digits)}`;
    }

    const padded = digits.padStart(places + 1, "0");
    const whole = padded.slice(0, -places);
    const fraction = padded.slice(-places).replace(/0+$/, "").slice(0, precision);
    return `${negative ? "-" : ""}${formatNumericString(whole)}${fraction ? `.${fraction}` : ""}`;
  } catch (_error) {
    return raw;
  }
}

function decodeCallData(inputData) {
  const normalized = with0x(inputData);
  if (!/^0x[0-9a-f]{8,}$/.test(normalized)) {
    return null;
  }

  const selector = normalized.slice(0, 10);
  const spec = KNOWN_METHODS[selector];
  const body = normalized.slice(10);
  const slots = body.match(/.{1,64}/g) || [];

  if (!spec) {
    return { selector, label: null, args: [] };
  }

  const args = spec.args.map((arg, index) => decodeInputArg(arg, slots[index]));
  return {
    selector,
    label: spec.label,
    args: args.filter(Boolean),
  };
}

function decodeInputArg(spec, slot) {
  if (!slot || slot.length < 64) {
    return null;
  }

  if (spec.type === "address") {
    const address = `0x${slot.slice(-40)}`;
    return {
      label: spec.label,
      type: spec.type,
      value: address,
      display: address,
      href: `/explore/address/${address}`,
    };
  }

  if (spec.type === "uint256") {
    try {
      const value = BigInt(`0x${slot}`).toString();
      return {
        label: spec.label,
        type: spec.type,
        value,
        display: formatNumericString(value),
      };
    } catch (_error) {
      return {
        label: spec.label,
        type: spec.type,
        value: `0x${slot}`,
        display: `0x${slot}`,
      };
    }
  }

  return {
    label: spec.label,
    type: spec.type,
    value: `0x${slot}`,
    display: `0x${slot}`,
  };
}

function formatTimestamp(value) {
  if (!value) {
    return "-";
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return String(value);
  }
  return date.toLocaleString("en-US", {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    timeZoneName: "short",
  });
}

function formatRelativeTime(value) {
  if (!value) {
    return "-";
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return String(value);
  }

  const deltaMs = date.getTime() - Date.now();
  const abs = Math.abs(deltaMs);
  const minute = 60_000;
  const hour = 60 * minute;
  const day = 24 * hour;
  const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

  if (abs < hour) {
    return rtf.format(Math.round(deltaMs / minute), "minute");
  }
  if (abs < day) {
    return rtf.format(Math.round(deltaMs / hour), "hour");
  }
  return rtf.format(Math.round(deltaMs / day), "day");
}

function formatRate(value) {
  const num = Number(value);
  if (!Number.isFinite(num)) {
    return String(value ?? "-");
  }
  if (num > 0 && num < 0.1) {
    return "<0.1";
  }
  return num.toFixed(num >= 100 ? 0 : 1);
}

function preferredSyncRate(status) {
  const postgresRate = Number(status?.postgres?.rate);
  if (Number.isFinite(postgresRate) && postgresRate > 0) {
    return postgresRate;
  }

  const clickhouseRate = Number(status?.clickhouse?.rate);
  if (Number.isFinite(clickhouseRate) && clickhouseRate > 0) {
    return clickhouseRate;
  }

  const syncRate = Number(status?.sync_rate);
  if (Number.isFinite(syncRate) && syncRate > 0) {
    return syncRate;
  }

  return null;
}

function parsePositiveInt(value, fallback) {
  const parsed = Number.parseInt(String(value || ""), 10);
  if (!Number.isFinite(parsed) || parsed < 1) {
    return fallback;
  }
  return parsed;
}

function optionalFieldValue(value) {
  const trimmed = String(value || "").trim();
  return trimmed ? trimmed : null;
}

function optionalNumberValue(value) {
  const trimmed = String(value || "").trim();
  if (!trimmed) {
    return null;
  }
  const parsed = Number.parseInt(trimmed, 10);
  return Number.isFinite(parsed) ? parsed : null;
}

function buildUrl(path, params = {}) {
  const search = new URLSearchParams();
  Object.entries(params).forEach(([key, value]) => {
    if (value !== null && value !== undefined && value !== "" && !(key === "page" && Number(value) === 1)) {
      search.set(key, String(value));
    }
  });
  const qs = search.toString();
  return qs ? `${path}?${qs}` : path;
}

function renderNotFound(message = "This explorer route is not available yet.") {
  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <section class="not-found-card">
        <div class="not-found-copy">${escapeHtml(message)}</div>
      </section>
    </section>
  `;
}

function renderError(error) {
  const message = error instanceof Error ? error.message : String(error);
  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <section class="not-found-card">
        <div class="not-found-copy">Explorer failed to load: ${escapeHtml(message)}</div>
      </section>
    </section>
  `;
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}
