// Minimal jsx-runtime shim for @xyflow/react UMD bundle.
// The UMD bundle expects a global `jsxRuntime` with `jsx` and `jsxs`
// functions. Both are just thin wrappers around React.createElement.
// Source: react/cjs/react-jsx-runtime.production.min.js
(function (global) {
  var React = global.React;
  var hasOwn = Object.prototype.hasOwnProperty;
  var REACT_ELEMENT_TYPE = Symbol.for("react.element");
  var REACT_FRAGMENT_TYPE = Symbol.for("react.fragment");
  var ReactCurrentOwner = React.__SECRET_INTERNALS_DO_NOT_USE_OR_YOU_WILL_BE_FIRED.ReactCurrentOwner;

  var RESERVED_PROPS = { key: true, ref: true, __self: true, __source: true };

  function hasValidRef(config) {
    return config.ref !== undefined;
  }

  function createElement(type, config, maybeKey) {
    var propName;
    var props = {};
    var key = null;
    var ref = null;

    if (maybeKey !== undefined) {
      key = "" + maybeKey;
    }
    if (config != null) {
      if (hasValidRef(config)) {
        ref = config.ref;
      }
      for (propName in config) {
        if (hasOwn.call(config, propName) && !RESERVED_PROPS.hasOwnProperty(propName)) {
          props[propName] = config[propName];
        }
      }
    }

    // Resolve default props
    if (type && type.defaultProps) {
      var defaultProps = type.defaultProps;
      for (propName in defaultProps) {
        if (props[propName] === undefined) {
          props[propName] = defaultProps[propName];
        }
      }
    }

    return {
      $$typeof: REACT_ELEMENT_TYPE,
      type: type,
      key: key,
      ref: ref,
      props: props,
      _owner: ReactCurrentOwner.current,
    };
  }

  global.jsxRuntime = {
    Fragment: REACT_FRAGMENT_TYPE,
    jsx: createElement,
    jsxs: createElement,
  };
})(typeof globalThis !== "undefined" ? globalThis : this);
