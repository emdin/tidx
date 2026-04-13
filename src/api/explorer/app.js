const PAGE_SIZE = 25;
const AUTO_REFRESH_MS = 10_000;

const TOPICS = {
  transfer: "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
  approval: "0x8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925",
};

const KNOWN_EVENTS = {
  [TOPICS.transfer]: "Transfer",
  [TOPICS.approval]: "Approval",
};

const state = {
  chainId: Number(window.__TIDX_DEFAULT_CHAIN_ID__) || null,
  status: null,
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
  const syncRate = state.status?.sync_rate ? `${formatRate(state.status.sync_rate)} blk/s` : null;

  const statusParts = [
    `Chain ${formatNumber(state.chainId || 0)}`,
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

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <span class="status-pill">Live</span>
          <span class="muted-badge mono">Chain ${formatNumber(state.chainId || 0)}</span>
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
          ${renderSummaryRow("Gas used", formatNumber(block.gas_used))}
          ${renderSummaryRow("Gas limit", formatNumber(block.gas_limit))}
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
}

async function renderReceiptPage(hash) {
  if (!isHex(hash, 64)) {
    renderNotFound("Invalid transaction hash.");
    return;
  }

  const body = hexBody(hash);
  const [txRows, logRows] = await Promise.all([
    runQuery(`
      SELECT
        txs.block_num,
        txs.block_timestamp,
        txs.idx,
        encode(txs.hash, 'hex') AS hash,
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
  ]);

  const tx = txRows[0];
  if (!tx) {
    renderNotFound(`Receipt ${escapeHtml(hash)} was not found in the local index.`);
    return;
  }

  setDocumentTitle(`Receipt ${shortHex(hash)} · Igra Explorer`);

  const statusKind = tx.status === null ? "pending" : Number(tx.status) === 1 ? "success" : "failed";
  const gasFee = multiplyNumericStrings(tx.gas_used, tx.effective_gas_price);

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
        ${renderKpiCard("Gas used", formatNumber(tx.gas_used), "Execution gas consumed")}
        ${renderKpiCard("Fee", gasFee ? formatNumericString(gasFee) : "-", "gas_used × effective_gas_price")}
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
          ${renderSummaryRow("Value", formatNumericString(tx.value || "0"))}
          ${renderSummaryRow("Gas limit", formatNumber(tx.gas_limit))}
          ${renderSummaryRow("Gas price", tx.effective_gas_price ? formatNumericString(tx.effective_gas_price) : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Cumulative gas", tx.cumulative_gas_used ? formatNumber(tx.cumulative_gas_used) : '<span class="text-secondary">-</span>')}
        </aside>

        <section class="panel-card">
          <div class="panel-header">
            <div>
              <div class="panel-title">Logs</div>
              <div class="panel-subtitle">${formatNumber(logRows.length)} log(s) recorded for this receipt</div>
            </div>
          </div>
          <div class="panel-body table-wrap">
            ${renderLogsTable(logRows)}
          </div>
        </section>
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

  const summaryRows = await runQuery(`
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
  `);

  const summary = summaryRows[0] || {};
  const kind = classifyAddress(summary);
  const tab = normalizeAddressTab(requestedTab, kind);
  const offset = (page - 1) * PAGE_SIZE;

  setDocumentTitle(`${kind.title} ${shortHex(address)} · Igra Explorer`);

  let panelTitle = "Transactions";
  let panelSubtitle = "";
  let panelBody = "";
  let paginationPath = `/explore/address/${address}`;
  let paginationExtras = { tab };
  let hasNextPage = false;

  if (tab === "transactions") {
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
  } else if (tab === "logs") {
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
    const contractLogs = await runQuery(`
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
    `);

    panelTitle = "Contract";
    panelSubtitle = kind.isContract
      ? "Inferred from creation receipts and/or logs emitted by this address"
      : "This address does not currently look like a contract from indexed data";
    panelBody = renderContractPanel(kind, summary, contractLogs);
    hasNextPage = false;
  } else {
    const [tokenSummaryRows, tokenEvents] = await Promise.all([
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
      runQuery(`
        SELECT
          block_num,
          block_timestamp,
          log_idx,
          encode(tx_hash, 'hex') AS tx_hash,
          encode(topic0, 'hex') AS topic0,
          encode(topic1, 'hex') AS topic1,
          encode(topic2, 'hex') AS topic2
        FROM logs
        WHERE address = decode('${body}', 'hex')
          AND (
            topic0 = decode('${hexBody(TOPICS.transfer)}', 'hex')
            OR topic0 = decode('${hexBody(TOPICS.approval)}', 'hex')
          )
        ORDER BY block_num DESC, log_idx DESC
        LIMIT ${PAGE_SIZE}
        OFFSET ${offset}
      `),
    ]);

    const tokenSummary = tokenSummaryRows[0] || {};
    panelTitle = "Token";
    panelSubtitle = kind.isToken
      ? "Token-like contract inferred from Transfer/Approval event signatures"
      : "This address is not currently classified as token-like";
    panelBody = renderTokenPanel(kind, tokenSummary, tokenEvents);
    hasNextPage = kind.isToken && tokenEvents.length === PAGE_SIZE;
  }

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <a class="muted-badge" href="/explore">${icons.arrow} Search</a>
          <span class="muted-badge">${kind.badge}</span>
          ${kind.isToken ? `<a class="muted-badge" href="/explore/token/${address}">${icons.tokens} Token view</a>` : ""}
        </div>
        <div>
          <h1 class="page-heading">${kind.title}</h1>
          <p class="page-subheading mono wrap-anywhere">${escapeHtml(address)}</p>
        </div>
      </header>

      <section class="kpi-grid">
        ${renderKpiCard("Transactions", formatNumber(summary.tx_count || 0), "Direct tx activity")}
        ${renderKpiCard("Logs", formatNumber(summary.related_logs || 0), "Logs emitted or indexed to this address")}
        ${renderKpiCard("First seen", summary.first_seen_block ? formatNumber(summary.first_seen_block) : "-", "Earliest indexed block")}
        ${renderKpiCard("Last seen", summary.last_seen_block ? formatNumber(summary.last_seen_block) : "-", "Latest indexed block")}
      </section>

      <div class="page-grid">
        <aside class="summary-card">
          ${renderSummaryRow("Address", escapeHtml(address))}
          ${renderSummaryRow("Kind", escapeHtml(kind.badge))}
          ${renderSummaryRow("Transactions", formatNumber(summary.tx_count || 0))}
          ${renderSummaryRow("Sent", formatNumber(summary.sent_count || 0))}
          ${renderSummaryRow("Received", formatNumber(summary.received_count || 0))}
          ${renderSummaryRow("First seen", summary.first_seen_block ? `<a href="/explore/block/${summary.first_seen_block}">${formatNumber(summary.first_seen_block)}</a>` : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Last seen", summary.last_seen_block ? `<a href="/explore/block/${summary.last_seen_block}">${formatNumber(summary.last_seen_block)}</a>` : '<span class="text-secondary">-</span>')}
          ${renderSummaryRow("Created as contract", summary.was_created ? "Yes" : "No")}
          ${renderSummaryRow("Created block", summary.created_block ? `<a href="/explore/block/${summary.created_block}">${formatNumber(summary.created_block)}</a>` : '<span class="text-secondary">-</span>')}
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
    </section>
  `;
}

async function renderTokensPage(page) {
  setDocumentTitle("Tokens · Igra Explorer");

  const offset = (page - 1) * PAGE_SIZE;
  const tokenRows = await runQuery(`
    SELECT
      encode(address, 'hex') AS address,
      COUNT(*) FILTER (WHERE topic0 = decode('${hexBody(TOPICS.transfer)}', 'hex')) AS transfer_count,
      COUNT(*) FILTER (WHERE topic0 = decode('${hexBody(TOPICS.approval)}', 'hex')) AS approval_count,
      MAX(block_num) AS last_seen_block
    FROM logs
    WHERE topic0 = decode('${hexBody(TOPICS.transfer)}', 'hex')
       OR topic0 = decode('${hexBody(TOPICS.approval)}', 'hex')
    GROUP BY address
    ORDER BY transfer_count DESC, approval_count DESC, last_seen_block DESC
    LIMIT ${PAGE_SIZE}
    OFFSET ${offset}
  `);

  elements.pageRoot.innerHTML = `
    <section class="content-page">
      <header class="page-header">
        <div class="badge-row">
          <span class="status-pill">Inferred</span>
          <span class="muted-badge">Transfer and Approval event signatures</span>
        </div>
        <div>
          <h1 class="page-heading">Tokens</h1>
          <p class="page-subheading">Token-like contracts inferred from indexed logs. Names and decimals are not available yet.</p>
        </div>
      </header>

      <section class="panel-card">
        <div class="panel-header">
          <div>
            <div class="panel-title">Token-like contracts</div>
            <div class="panel-subtitle">Addresses emitting ERC-20-style Transfer or Approval events</div>
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
          <td class="align-right mono">${formatNumericString(row.value || "0")}</td>
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
          <td class="align-right mono">${formatNumericString(row.value || "0")}</td>
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

function renderContractPanel(kind, summary, rows) {
  if (!kind.isContract) {
    return `
      <div class="notice-card">
        <div class="notice-copy">
          This address does not currently have contract-like signals in the indexed data. If your RPC exposes
          eth_getCode, we can add explicit bytecode-based detection next.
        </div>
      </div>
    `;
  }

  return `
    <div class="panel-stack">
      <div class="kpi-grid kpi-grid-tight">
        ${renderKpiCard("Created block", summary.created_block ? formatNumber(summary.created_block) : "-", "Contract deployment")}
        ${renderKpiCard("Emitted logs", formatNumber(summary.emitted_logs || 0), "Logs from this address")}
        ${renderKpiCard("Transfer logs", formatNumber(summary.transfer_logs || 0), "Transfer signature hits")}
      </div>
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

function renderTokenPanel(kind, tokenSummary, rows) {
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

  return `
    <div class="panel-stack">
      <div class="kpi-grid kpi-grid-tight">
        ${renderKpiCard("Transfers", formatNumber(tokenSummary.transfer_count || 0), "Transfer event count")}
        ${renderKpiCard("Approvals", formatNumber(tokenSummary.approval_count || 0), "Approval event count")}
        ${renderKpiCard("Participants", formatNumber(tokenSummary.participant_count || 0), "Distinct indexed addresses")}
      </div>
      <div class="table-wrap">
        <table class="data-table">
          <thead>
            <tr>
              <th>Block</th>
              <th>Time</th>
              <th>Tx</th>
              <th>Event</th>
              <th>From / Owner</th>
              <th>To / Spender</th>
            </tr>
          </thead>
          <tbody>
            ${
              rows.length
                ? rows
                    .map((row) => {
                      const event = eventName(row.topic0);
                      const first = topicToAddress(row.topic1);
                      const second = topicToAddress(row.topic2);
                      return `
                        <tr>
                          <td class="mono"><a href="/explore/block/${row.block_num}">${formatNumber(row.block_num)}</a></td>
                          <td>${escapeHtml(formatRelativeTime(row.block_timestamp))}</td>
                          <td class="mono"><a href="/explore/receipt/${with0x(row.tx_hash)}">${escapeHtml(shortHex(with0x(row.tx_hash), 10))}</a></td>
                          <td>${escapeHtml(event)}</td>
                          <td class="mono">${first ? `<a href="/explore/address/${first}">${escapeHtml(shortHex(first, 8))}</a>` : '<span class="text-secondary">-</span>'}</td>
                          <td class="mono">${second ? `<a href="/explore/address/${second}">${escapeHtml(shortHex(second, 8))}</a>` : '<span class="text-secondary">-</span>'}</td>
                        </tr>
                      `;
                    })
                    .join("")
                : '<tr><td colspan="6" class="text-secondary">No token-like events found.</td></tr>'
            }
          </tbody>
        </table>
      </div>
    </div>
  `;
}

function renderTokensTable(rows) {
  if (!rows.length) {
    return '<div class="empty-state">No token-like contracts inferred yet.</div>';
  }

  const body = rows
    .map(
      (row) => `
        <tr>
          <td class="mono"><a href="/explore/token/${with0x(row.address)}">${escapeHtml(with0x(row.address))}</a></td>
          <td class="align-right mono">${formatNumber(row.transfer_count || 0)}</td>
          <td class="align-right mono">${formatNumber(row.approval_count || 0)}</td>
          <td class="align-right mono"><a href="/explore/block/${row.last_seen_block}">${formatNumber(row.last_seen_block)}</a></td>
        </tr>
      `,
    )
    .join("");

  return `
    <table class="data-table">
      <thead>
        <tr>
          <th>Address</th>
          <th class="align-right">Transfers</th>
          <th class="align-right">Approvals</th>
          <th class="align-right">Last seen</th>
        </tr>
      </thead>
      <tbody>${body}</tbody>
    </table>
  `;
}

function renderLogsTable(rows) {
  if (!rows.length) {
    return '<div class="empty-state">No logs recorded for this receipt.</div>';
  }

  const body = rows
    .map(
      (row) => `
        <tr>
          <td class="mono">${formatNumber(row.log_idx)}</td>
          <td class="mono"><a href="/explore/address/${with0x(row.address)}?tab=contract">${escapeHtml(shortHex(with0x(row.address), 8))}</a></td>
          <td>${escapeHtml(eventName(row.topic0))}</td>
          <td class="mono">${row.topic1 ? escapeHtml(shortHex(with0x(row.topic1), 10)) : '<span class="text-secondary">-</span>'}</td>
          <td class="mono">${row.topic2 ? escapeHtml(shortHex(with0x(row.topic2), 10)) : '<span class="text-secondary">-</span>'}</td>
          <td class="align-right mono">${formatNumber(row.data_length || 0)}</td>
        </tr>
      `,
    )
    .join("");

  return `
    <div class="table-wrap">
      <table class="data-table">
        <thead>
          <tr>
            <th>Index</th>
            <th>Address</th>
            <th>Event</th>
            <th>Topic1</th>
            <th>Topic2</th>
            <th class="align-right">Data bytes</th>
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
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      const input = form.querySelector("input[name='explore-query']");
      const value = input?.value?.trim() || "";
      const target = resolveSearchTarget(value);
      if (!target) {
        input?.focus();
        return;
      }
      window.location.href = target;
    });
  });
}

function resolveSearchTarget(value) {
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

  return "/explore/tokens";
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

function classifyAddress(summary) {
  const isContract = Boolean(summary.was_created) || Number(summary.emitted_logs || 0) > 0;
  const isToken = isContract && (Number(summary.transfer_logs || 0) > 0 || Number(summary.approval_logs || 0) > 0);

  if (isToken) {
    return { isContract: true, isToken: true, badge: "Token-like contract", title: "Token Contract" };
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

  const allowed = new Set(["transactions", "logs"]);
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
  const trimmed = String(value || "").trim();
  if (!trimmed) {
    return "";
  }
  return trimmed.startsWith("0x") ? trimmed.toLowerCase() : `0x${trimmed.toLowerCase()}`;
}

function with0x(value) {
  if (!value) {
    return "";
  }
  return value.startsWith("0x") ? value.toLowerCase() : `0x${String(value).toLowerCase()}`;
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
  return num.toFixed(num >= 100 ? 0 : 1);
}

function parsePositiveInt(value, fallback) {
  const parsed = Number.parseInt(String(value || ""), 10);
  if (!Number.isFinite(parsed) || parsed < 1) {
    return fallback;
  }
  return parsed;
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
