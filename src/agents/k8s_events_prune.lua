-- Copyright (C) 2026 Tornis Desenvolvimento
-- SPDX-License-Identifier: AGPL-3.0-only
--
-- k8s-events collector filter (#104). kubernetes_events records carry
-- metadata.managedFields (server-side-apply bookkeeping, full of empty maps
-- such as "f:host": {}) and other empty maps that flatten to dot-only field
-- names, which OpenSearch rejects with mapper_parsing_exception — every chunk
-- 400s and nothing is ever indexed. Drop managedFields and prune empty maps.
--
-- Static engine content, never interpolated (ADR-039): shipped verbatim in the
-- collector ConfigMap and mounted at /fluent-bit/etc/prune.lua.
function prune(tag, timestamp, record)
    function clean(t)
        for k, v in pairs(t) do
            if type(v) == "table" then
                clean(v)
                if next(v) == nil then t[k] = nil end
            end
        end
    end
    if type(record["metadata"]) == "table" then
        record["metadata"]["managedFields"] = nil
    end
    clean(record)
    if next(record) == nil then
        return -1, timestamp, record
    end
    return 1, timestamp, record
end
