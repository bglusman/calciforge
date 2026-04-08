"use strict";
// before_tool_call hook handler
// This is invoked by OpenClaw before each tool execution
var __awaiter = (this && this.__awaiter) || function (thisArg, _arguments, P, generator) {
    function adopt(value) { return value instanceof P ? value : new P(function (resolve) { resolve(value); }); }
    return new (P || (P = Promise))(function (resolve, reject) {
        function fulfilled(value) { try { step(generator.next(value)); } catch (e) { reject(e); } }
        function rejected(value) { try { step(generator["throw"](value)); } catch (e) { reject(e); } }
        function step(result) { result.done ? resolve(result.value) : adopt(result.value).then(fulfilled, rejected); }
        step((generator = generator.apply(thisArg, _arguments || [])).next());
    });
};
var __generator = (this && this.__generator) || function (thisArg, body) {
    var _ = { label: 0, sent: function() { if (t[0] & 1) throw t[1]; return t[1]; }, trys: [], ops: [] }, f, y, t, g = Object.create((typeof Iterator === "function" ? Iterator : Object).prototype);
    return g.next = verb(0), g["throw"] = verb(1), g["return"] = verb(2), typeof Symbol === "function" && (g[Symbol.iterator] = function() { return this; }), g;
    function verb(n) { return function (v) { return step([n, v]); }; }
    function step(op) {
        if (f) throw new TypeError("Generator is already executing.");
        while (g && (g = 0, op[0] && (_ = 0)), _) try {
            if (f = 1, y && (t = op[0] & 2 ? y["return"] : op[0] ? y["throw"] || ((t = y["return"]) && t.call(y), 0) : y.next) && !(t = t.call(y, op[1])).done) return t;
            if (y = 0, t) op = [op[0] & 2, t.value];
            switch (op[0]) {
                case 0: case 1: t = op; break;
                case 4: _.label++; return { value: op[1], done: false };
                case 5: _.label++; y = op[1]; op = [0]; continue;
                case 7: op = _.ops.pop(); _.trys.pop(); continue;
                default:
                    if (!(t = _.trys, t = t.length > 0 && t[t.length - 1]) && (op[0] === 6 || op[0] === 2)) { _ = 0; continue; }
                    if (op[0] === 3 && (!t || (op[1] > t[0] && op[1] < t[3]))) { _.label = op[1]; break; }
                    if (op[0] === 6 && _.label < t[1]) { _.label = t[1]; t = op; break; }
                    if (t && _.label < t[2]) { _.label = t[2]; _.ops.push(op); break; }
                    if (t[2]) _.ops.pop();
                    _.trys.pop(); continue;
            }
            op = body.call(thisArg, _);
        } catch (e) { op = [6, e]; y = 0; } finally { f = t = 0; }
        if (op[0] & 5) throw op[1]; return { value: op[0] ? op[1] : void 0, done: true };
    }
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.default = beforeToolCall;
var CLASHD_ENDPOINT = process.env.CLASHD_ENDPOINT || "http://localhost:9001/evaluate";
var CLASHD_TIMEOUT_MS = parseInt(process.env.CLASHD_TIMEOUT_MS || "500", 10);
function beforeToolCall(context) {
    return __awaiter(this, void 0, void 0, function () {
        var toolName, args, identity, controller_1, timeoutId, response, result, error_1;
        var _a;
        return __generator(this, function (_b) {
            switch (_b.label) {
                case 0:
                    toolName = context.toolName;
                    args = context.args;
                    identity = ((_a = context.session) === null || _a === void 0 ? void 0 : _a.identity) || "unknown";
                    console.log("[zeroclawed-policy] Evaluating: ".concat(toolName, " for ").concat(identity));
                    _b.label = 1;
                case 1:
                    _b.trys.push([1, 4, , 5]);
                    controller_1 = new AbortController();
                    timeoutId = setTimeout(function () { return controller_1.abort(); }, CLASHD_TIMEOUT_MS);
                    return [4 /*yield*/, fetch(CLASHD_ENDPOINT, {
                            method: "POST",
                            headers: { "Content-Type": "application/json" },
                            body: JSON.stringify({
                                tool: toolName,
                                args: args,
                                context: { identity: identity, timestamp: new Date().toISOString() }
                            }),
                            signal: controller_1.signal
                        })];
                case 2:
                    response = _b.sent();
                    clearTimeout(timeoutId);
                    if (!response.ok) {
                        throw new Error("clashd returned ".concat(response.status));
                    }
                    return [4 /*yield*/, response.json()];
                case 3:
                    result = _b.sent();
                    if (result.verdict === "deny") {
                        return [2 /*return*/, { block: true, reason: result.reason || "Policy denied" }];
                    }
                    if (result.verdict === "review") {
                        return [2 /*return*/, { requireApproval: true, reason: result.reason || "Custodian approval required" }];
                    }
                    return [2 /*return*/, { block: false }];
                case 4:
                    error_1 = _b.sent();
                    console.error("[zeroclawed-policy] Error: ".concat(error_1));
                    // Fail-safe: deny on error
                    return [2 /*return*/, { block: true, reason: "Policy enforcement unavailable" }];
                case 5: return [2 /*return*/];
            }
        });
    });
}
